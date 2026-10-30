//! A serialized, local Bash session, currently supported only on macOS.
//!
//! Commands are evaluated in the same shell, preserving its working directory, variables,
//! exported environment, and functions. Stdin is `/dev/null`; this is not an interactive
//! terminal. Stdout and stderr share a bounded capture buffer. Shell `&` jobs are killed
//! and reaped before command completion; they do not persist into the next command. Use
//! the tool's background execution mode for long-running commands instead. This completion
//! cleanup covers Bash-tracked jobs, including pipelines, not disowned jobs or untracked
//! descendants of other programs. Such descendants must not retain the output descriptors.
//!
//! A private Unix socket carries command records and completion records, never stdout
//! markers. This is a lifecycle boundary, not a security boundary: evaluated shell code can
//! deliberately alter Bash's options, descriptors, or protocol. A lost shell, invalid
//! protocol, timeout, or close permanently ends the session. Ordinary nonzero exit codes
//! do not. `exit` and `exec` that replace the shell cannot complete normally.
//!
//! Cleanup kills the shell's process group, including ordinary background descendants,
//! and reaps the direct child. Descendants that deliberately escape with `setsid` or change
//! process groups are outside this guarantee. Cleanup waits at most two seconds for the
//! direct child and disappearance of the process group. If the OS cannot reap the child
//! in that time, a detached reaper retains ownership. `close` reports cleanup failures
//! rather than blocking the caller indefinitely.

use anyhow::Result;
#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

/// Maximum retained combined stdout/stderr bytes for the latest command.
pub const OUTPUT_LIMIT: usize = 64 * 1024;
/// Larger commands are rejected without changing session state.
pub const COMMAND_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ShellStatus {
    Idle,
    Running,
    Closed,
    Failed { reason: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum CommandStatus {
    Running,
    Completed { exit_code: i32 },
    TimedOut,
    Cancelled,
    Failed { reason: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandSnapshot {
    pub id: u64,
    pub status: CommandStatus,
    /// Combined stdout/stderr, decoded with replacement for invalid UTF-8.
    pub output: String,
    pub output_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ShellSnapshot {
    pub status: ShellStatus,
    /// Retained until the next successful submission, including after close.
    pub command: Option<CommandSnapshot>,
}

pub struct PersistentShell {
    #[cfg(target_os = "macos")]
    inner: macos::Session,
}

impl PersistentShell {
    pub fn spawn(cwd: PathBuf) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                inner: macos::Session::spawn(cwd)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop(cwd);
            bail!("persistent shell is currently supported only on macOS")
        }
    }

    /// Enqueues without waiting for execution. Rejects a second in-flight command,
    /// closed/failed sessions, zero or unrepresentable timeouts, NULs, and oversize input.
    /// The timeout includes time spent delivering the command to Bash.
    pub fn submit(&self, command: String, timeout: Duration) -> Result<u64> {
        #[cfg(target_os = "macos")]
        {
            self.inner.submit(command, timeout)
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop((command, timeout));
            bail!("persistent shell is currently supported only on macOS")
        }
    }

    pub fn snapshot(&self) -> ShellSnapshot {
        #[cfg(target_os = "macos")]
        {
            self.inner.snapshot()
        }
        #[cfg(not(target_os = "macos"))]
        {
            ShellSnapshot {
                status: ShellStatus::Failed {
                    reason: "persistent shell is currently supported only on macOS".into(),
                },
                command: None,
            }
        }
    }

    /// Synchronously stops the worker and cleans up its process group. Idempotent;
    /// subsequent calls return the same cleanup result. Running commands are cancelled.
    pub fn close(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.close()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }
}

impl Drop for PersistentShell {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            log::error!("persistent shell cleanup failed: {error:#}");
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use anyhow::{anyhow, bail};
    use std::{
        io::{self, Read, Write},
        os::{
            fd::{AsRawFd, OwnedFd},
            raw::c_int,
            unix::{net::UnixStream, process::CommandExt},
        },
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex, MutexGuard,
            mpsc::{self, Receiver, RecvTimeoutError, Sender},
        },
        thread::{self, JoinHandle},
        time::Instant,
    };

    const F_SETFD: c_int = 2;
    const SIGKILL: c_int = 9;
    const EPERM: i32 = 1;
    const ESRCH: i32 = 3;
    const TICK: Duration = Duration::from_millis(5);
    const CLEANUP_LIMIT: Duration = Duration::from_secs(2);
    const IO_BUDGET: usize = 256 * 1024;
    const P_PID: c_int = 1;
    const WEXITED: c_int = 0x4;
    const WNOHANG: c_int = 0x1;
    // Observe exit without reaping, so the shell PID keeps reserving its
    // process group until cleanup has signalled it. Reaping first would let the
    // PID (and therefore the process group) be reused under load; a later
    // killpg could then target an unrelated group.
    const WNOWAIT: c_int = 0x20;

    #[repr(C, align(8))]
    struct SigInfo {
        _signo: c_int,
        _errno: c_int,
        _code: c_int,
        pid: i32,
        _rest: [u8; 120],
    }

    unsafe extern "C" {
        fn fcntl(descriptor: c_int, command: c_int, ...) -> c_int;
        fn kill(pid: c_int, signal: c_int) -> c_int;
        fn getpgid(pid: c_int) -> c_int;
        fn waitid(idtype: c_int, id: u32, info: *mut SigInfo, options: c_int) -> c_int;
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        // Cleanup must still run if a caller unwinds while holding the state lock.
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct State {
        snapshot: ShellSnapshot,
        output: Vec<u8>,
        next_id: u64,
        cleanup_error: Option<String>,
    }

    enum Message {
        Submit {
            id: u64,
            command: String,
            deadline: Instant,
        },
        Close,
    }

    pub(super) struct Session {
        state: Arc<Mutex<State>>,
        sender: Sender<Message>,
        worker: Mutex<Option<JoinHandle<()>>>,
        #[cfg(test)]
        pub(super) control_descriptor: c_int,
    }

    impl Session {
        pub(super) fn spawn(cwd: PathBuf) -> Result<Self> {
            let (control, shell_control) = UnixStream::pair()?;
            let (output, shell_output) = UnixStream::pair()?;
            control.set_nonblocking(true)?;
            output.set_nonblocking(true)?;
            let control_descriptor = shell_control.as_raw_fd();
            if control_descriptor <= 2 {
                bail!("persistent shell requires the parent standard descriptors to be open");
            }
            // Use job specifications rather than `jobs -p` PIDs: with job control off,
            // killing a pipeline's leader alone leaves its other processes running.
            // The here-string keeps kill/wait in the owning shell, not a pipeline subshell.
            // Kill can race a job's normal exit; wait still reaps every tracked job.
            let script = format!(
                r#"__zed_persistent_complete() {{
    builtin local __zed_persistent_job_line __zed_persistent_job_number
    while IFS= builtin read -r __zed_persistent_job_line; do
        case "$__zed_persistent_job_line" in
            \[[0-9]*\]*)
                __zed_persistent_job_number=${{__zed_persistent_job_line%%]*}}
                __zed_persistent_job_number=${{__zed_persistent_job_number#\[}}
                if builtin kill -KILL "%$__zed_persistent_job_number" 2>/dev/null; then :; fi
                ;;
        esac
    done <<< "$(builtin jobs -l)"
    if builtin wait 2>/dev/null; then :; fi
    builtin printf '%s %s\n' "$1" "$2" >&{control_descriptor}
}}
while IFS= builtin read -r -u {control_descriptor} __zed_persistent_record; do
    builtin eval -- "$__zed_persistent_record"
done
"#
            );
            let stdout: OwnedFd = shell_output.try_clone()?.into();
            let stderr: OwnedFd = shell_output.into();
            let mut command = Command::new("/bin/bash");
            command
                .args(["--noprofile", "--norc", "-c", &script])
                .current_dir(cwd)
                .env_remove("BASH_ENV")
                .env_remove("ENV")
                .env_remove("SHELLOPTS")
                .env_remove("BASHOPTS")
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .process_group(0);
            // Keep the already-owned descriptor instead of dup2 to an arbitrary number:
            // that could overwrite Rust's private exec-error pipe in the child.
            // fcntl is async-signal-safe and only changes the child's descriptor flags.
            unsafe {
                command.pre_exec(move || {
                    if fcntl(control_descriptor, F_SETFD, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn()?;
            drop(command);
            drop(shell_control);
            let state = Arc::new(Mutex::new(State {
                snapshot: ShellSnapshot {
                    status: ShellStatus::Idle,
                    command: None,
                },
                output: Vec::new(),
                next_id: 1,
                cleanup_error: None,
            }));
            let (sender, receiver) = mpsc::channel();
            // The worker owns the process, not Session. There is no handle/thread cycle.
            // The process guard also cleans up if spawning the thread fails or it unwinds.
            let process = Process::new(child);
            let worker = thread::Builder::new()
                .name("persistent-shell".into())
                .spawn({
                    let state = state.clone();
                    move || run(process, control, output, receiver, state)
                })?;
            Ok(Self {
                state,
                sender,
                worker: Mutex::new(Some(worker)),
                #[cfg(test)]
                control_descriptor,
            })
        }

        pub(super) fn submit(&self, command: String, timeout: Duration) -> Result<u64> {
            if command.len() > COMMAND_LIMIT {
                bail!("command exceeds {COMMAND_LIMIT} bytes");
            }
            if command.contains('\0') {
                bail!("shell commands cannot contain NUL bytes");
            }
            if timeout.is_zero() {
                bail!("command timeout must be positive");
            }
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| anyhow!("command timeout is too large"))?;
            let mut state = lock(&self.state);
            if state.snapshot.status != ShellStatus::Idle {
                bail!("persistent shell is not idle: {:?}", state.snapshot.status);
            }
            let id = state.next_id;
            let next_id = id
                .checked_add(1)
                .ok_or_else(|| anyhow!("command id exhausted"))?;
            state.snapshot.status = ShellStatus::Running;
            state.snapshot.command = Some(CommandSnapshot {
                id,
                status: CommandStatus::Running,
                output: String::new(),
                output_truncated: false,
            });
            state.output.clear();
            state.next_id = next_id;
            if self
                .sender
                .send(Message::Submit {
                    id,
                    command,
                    deadline,
                })
                .is_err()
            {
                let reason = "persistent shell worker has stopped".to_string();
                fail(&mut state, reason.clone(), CommandStatus::Failed { reason });
                bail!("persistent shell worker has stopped");
            }
            Ok(id)
        }

        pub(super) fn snapshot(&self) -> ShellSnapshot {
            let state = lock(&self.state);
            let mut snapshot = state.snapshot.clone();
            if let Some(command) = &mut snapshot.command {
                command.output = String::from_utf8_lossy(&state.output).into_owned();
                if command.output.len() > OUTPUT_LIMIT {
                    let mut boundary = OUTPUT_LIMIT;
                    while !command.output.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    command.output.truncate(boundary);
                    command.output_truncated = true;
                }
            }
            snapshot
        }

        pub(super) fn close(&self) -> Result<()> {
            // Serializes concurrent closes, including their joins. Submit only takes state.
            let mut worker = lock(&self.worker);
            if let Some(worker) = worker.take() {
                {
                    let mut state = lock(&self.state);
                    if matches!(
                        state.snapshot.status,
                        ShellStatus::Idle | ShellStatus::Running
                    ) {
                        state.snapshot.status = ShellStatus::Closed;
                        if let Some(command) = &mut state.snapshot.command {
                            if command.status == CommandStatus::Running {
                                command.status = CommandStatus::Cancelled;
                            }
                        }
                    }
                }
                // A disconnected receiver means cleanup has already started or finished.
                if self.sender.send(Message::Close).is_err() {
                    log::debug!("persistent shell worker already stopped during close");
                }
                if worker.join().is_err() {
                    let mut state = lock(&self.state);
                    state.cleanup_error = Some("persistent shell worker panicked".into());
                    let reason = "persistent shell worker panicked".to_string();
                    fail(&mut state, reason.clone(), CommandStatus::Failed { reason });
                }
            }
            if let Some(error) = &lock(&self.state).cleanup_error {
                bail!("{error}");
            }
            Ok(())
        }
    }

    struct Process {
        child: Option<Child>,
        group: c_int,
    }

    impl Process {
        fn new(child: Child) -> Self {
            Self {
                group: child.id() as c_int,
                child: Some(child),
            }
        }

        fn has_exited(&self) -> io::Result<bool> {
            let Some(child) = &self.child else {
                return Ok(true);
            };
            let mut info = SigInfo {
                _signo: 0,
                _errno: 0,
                _code: 0,
                pid: 0,
                _rest: [0; 120],
            };
            let result =
                unsafe { waitid(P_PID, child.id(), &mut info, WEXITED | WNOHANG | WNOWAIT) };
            if result == 0 {
                Ok(info.pid == child.id() as i32)
            } else {
                Err(io::Error::last_os_error())
            }
        }

        fn cleanup(&mut self) -> Result<()> {
            let Some(mut child) = self.child.take() else {
                return Ok(());
            };
            // Even if Bash has already exited, its background children may still be alive.
            let actual_group = unsafe { getpgid(self.group) };
            let group_error = if unsafe { kill(-self.group, SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                match error.raw_os_error() {
                    // The group is already gone, or the id was reused by a group
                    // we do not own. Either way there is nothing left to signal.
                    Some(ESRCH) | Some(EPERM) => None,
                    _ => Some((error, actual_group)),
                }
            } else {
                None
            };
            let deadline = Instant::now() + CLEANUP_LIMIT;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => thread::sleep(TICK),
                    result => {
                        // Retain responsibility for wait() without extending close's bound.
                        thread::spawn(move || {
                            if let Err(error) = child.wait() {
                                log::error!("persistent shell deferred reap failed: {error}");
                            }
                        });
                        match result {
                            Err(error) => return Err(error.into()),
                            _ => bail!("persistent shell did not exit within cleanup deadline"),
                        }
                    }
                }
            }
            if let Some((error, actual_group)) = group_error {
                return Err(anyhow!(
                    "killpg({}, actual group {actual_group}) failed: {error}",
                    self.group
                ));
            }
            loop {
                if unsafe { kill(-self.group, 0) } == -1 {
                    let error = io::Error::last_os_error();
                    if matches!(error.raw_os_error(), Some(ESRCH)) {
                        break;
                    }
                    if error.raw_os_error() == Some(EPERM) {
                        // The id no longer names our process group. This can
                        // happen once the leader is reaped and the pid is reused.
                        log::warn!(
                            "persistent shell process group {} is no longer signalable: {error}",
                            self.group
                        );
                        break;
                    }
                    return Err(anyhow!(
                        "probing process group {} failed: {error}",
                        self.group
                    ));
                }
                if Instant::now() >= deadline {
                    bail!("persistent shell process group survived the cleanup deadline");
                }
                thread::sleep(TICK);
            }
            Ok(())
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            if let Err(error) = self.cleanup() {
                log::error!("persistent shell process cleanup failed: {error:#}");
            }
        }
    }

    struct Active {
        id: u64,
        deadline: Instant,
        record: Vec<u8>,
        written: usize,
        completion: Vec<u8>,
        exit_code: Option<i32>,
    }

    fn fail(state: &mut State, reason: String, command_status: CommandStatus) {
        state.snapshot.status = ShellStatus::Failed { reason };
        if let Some(command) = &mut state.snapshot.command {
            if command.status == CommandStatus::Running {
                command.status = command_status;
            }
        }
    }

    fn quote(command: &str) -> String {
        // Each control record is one physical line, even for multiline shell programs.
        format!(
            "'{}'",
            command.replace('\'', "'\\''").replace('\n', "'$'\\n''")
        )
    }

    fn run(
        mut process: Process,
        mut control: UnixStream,
        mut output: UnixStream,
        receiver: Receiver<Message>,
        state: Arc<Mutex<State>>,
    ) {
        let mut active: Option<Active> = None;
        let outcome: Result<()> = (|| {
            loop {
                if lock(&state).snapshot.status == ShellStatus::Closed {
                    return Ok(());
                }
                match receiver.recv_timeout(if active.is_some() {
                    Duration::ZERO
                } else {
                    TICK
                }) {
                    Ok(Message::Close) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Ok(Message::Submit {
                        id,
                        command,
                        deadline,
                    }) => {
                        if active.is_some() {
                            bail!("received concurrent persistent shell commands");
                        }
                        // The nested eval keeps stdin redirection local to this command.
                        // Capture $? as a function argument before cleanup changes it.
                        let record = format!(
                            "builtin eval -- {} </dev/null; __zed_persistent_complete '{}' \"$?\"\n",
                            quote(&command),
                            id,
                        );
                        active = Some(Active {
                            id,
                            deadline,
                            record: record.into_bytes(),
                            written: 0,
                            completion: Vec::new(),
                            exit_code: None,
                        });
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
                // Read output before observing exit or control EOF, so even an immediate
                // `exit` retains diagnostics already written by Bash and its children.
                let drained = drain_output(&mut output, &state, active.is_some())?;
                if process.has_exited()? {
                    bail!("persistent shell exited unexpectedly");
                }
                let Some(command) = &mut active else {
                    continue;
                };
                if Instant::now() >= command.deadline {
                    let mut state = lock(&state);
                    if state.snapshot.status != ShellStatus::Closed {
                        fail(
                            &mut state,
                            "command timed out; session terminated".into(),
                            CommandStatus::TimedOut,
                        );
                    }
                    return Ok(());
                }
                let end = command.record.len().min(command.written + IO_BUDGET);
                while command.written < end {
                    match control.write(&command.record[command.written..end]) {
                        Ok(0) => bail!("persistent shell control socket closed while writing"),
                        Ok(count) => command.written += count,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                        Err(error) => return Err(error.into()),
                    }
                }
                read_completion(&mut control, command)?;
                // Completion may have arrived after the first drain. Drain again after
                // reading it before publishing Idle, so the next submit cannot steal output.
                let drained = drain_output(&mut output, &state, true)? && drained;
                if let Some(exit_code) = command.exit_code {
                    if drained {
                        let mut state = lock(&state);
                        // Close may have won the race while this worker was doing I/O.
                        if state.snapshot.status == ShellStatus::Running {
                            if let Some(snapshot) = &mut state.snapshot.command {
                                snapshot.status = CommandStatus::Completed { exit_code };
                            }
                            state.snapshot.status = ShellStatus::Idle;
                        }
                        active = None;
                    }
                }
                // All descriptors are nonblocking and each turn has an I/O budget, so
                // output floods cannot starve deadlines or close requests.
                thread::sleep(TICK);
            }
        })();
        if let Err(error) = outcome {
            let mut state = lock(&state);
            if state.snapshot.status != ShellStatus::Closed {
                let reason = format!("{error:#}");
                fail(&mut state, reason.clone(), CommandStatus::Failed { reason });
            }
        }
        if let Err(error) = process.cleanup() {
            let mut state = lock(&state);
            state.cleanup_error = Some(format!("{error:#}"));
            let reason = format!("persistent shell cleanup failed: {error:#}");
            fail(&mut state, reason.clone(), CommandStatus::Failed { reason });
        }
        // The shell can exit between a drain and try_wait/control EOF. After cleanup,
        // retain any final diagnostics without waiting for escaped descriptor holders.
        let capture = lock(&state).snapshot.command.is_some();
        if let Err(error) = drain_output(&mut output, &state, capture) {
            log::warn!("persistent shell final output drain failed: {error:#}");
        }
    }

    fn read_completion(control: &mut UnixStream, command: &mut Active) -> Result<()> {
        let mut buffer = [0; 128];
        match control.read(&mut buffer) {
            Ok(0) => bail!("persistent shell control socket closed"),
            Ok(count) => {
                command.completion.extend_from_slice(&buffer[..count]);
                if command.completion.len() > 128 {
                    bail!("oversize persistent shell completion record");
                }
                if command.completion.contains(&b'\n') {
                    let completion = std::str::from_utf8(&command.completion)?;
                    let Some((id, code)) = completion
                        .strip_suffix('\n')
                        .and_then(|line| line.split_once(' '))
                    else {
                        bail!("invalid persistent shell completion record");
                    };
                    let id: u64 = id.parse()?;
                    let exit_code: i32 = code.parse()?;
                    if id != command.id
                        || !(0..=255).contains(&exit_code)
                        || command.exit_code.is_some()
                    {
                        bail!("invalid persistent shell completion id or exit code");
                    }
                    command.exit_code = Some(exit_code);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn drain_output(output: &mut UnixStream, state: &Mutex<State>, capture: bool) -> Result<bool> {
        let mut buffer = [0; 8192];
        let mut drained = 0;
        while drained < IO_BUDGET {
            match output.read(&mut buffer) {
                Ok(0) => return Ok(true),
                Ok(count) => {
                    drained += count;
                    if capture {
                        let mut state = lock(state);
                        let retained = count.min(OUTPUT_LIMIT - state.output.len());
                        state.output.extend_from_slice(&buffer[..retained]);
                        if retained < count {
                            if let Some(command) = &mut state.snapshot.command {
                                command.output_truncated = true;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    #[cfg(test)]
    pub(super) fn process_exists(pid: i32) -> bool {
        if unsafe { kill(pid, 0) } == 0 {
            return true;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(ESRCH) => false,
            // A negative pid names a process group. EPERM means that group id
            // no longer belongs to us (typically reused after the leader was
            // reaped), so the group we cared about is gone.
            Some(EPERM) if pid < 0 => false,
            _ => true,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use std::{fs, thread, time::Instant};
    use tempfile::TempDir;

    fn wait_for(
        shell: &PersistentShell,
        predicate: impl Fn(&ShellSnapshot) -> bool,
    ) -> ShellSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = shell.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "snapshot did not settle: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn completed(shell: &PersistentShell) -> CommandSnapshot {
        let snapshot = wait_for(shell, |snapshot| snapshot.status != ShellStatus::Running);
        assert_eq!(snapshot.status, ShellStatus::Idle, "{snapshot:?}");
        snapshot.command.expect("submitted command")
    }

    fn wait_gone(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while macos::process_exists(pid) {
            assert!(
                Instant::now() < deadline,
                "process/group {pid} survived cleanup"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn child_ids(shell: &PersistentShell) -> (i32, i32) {
        let snapshot = wait_for(shell, |snapshot| {
            snapshot
                .command
                .as_ref()
                .is_some_and(|command| command.output.lines().count() >= 2)
        });
        let output = snapshot.command.expect("running command").output;
        let mut lines = output.lines();
        let shell_pid = lines
            .next()
            .expect("shell pid")
            .parse()
            .expect("numeric shell pid");
        let child_pid = lines
            .next()
            .expect("child pid")
            .parse()
            .expect("numeric child pid");
        (shell_pid, child_pid)
    }

    const CHILD_COMMAND: &str =
        "printf '%s\\n' \"$$\"; /bin/sleep 60 & printf '%s\\n' \"$!\"; wait";

    #[test]
    fn preserves_shell_state_cwd_and_environment() -> Result<()> {
        let directory = TempDir::new()?;
        fs::create_dir(directory.path().join("nested"))?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        let first = shell.submit(
            "cd nested\nvalue='hello '\"world\"\nexport EXPORTED=environment\nfunction persistent_function { printf function; }".into(),
            Duration::from_secs(5),
        )?;
        assert_eq!(
            completed(&shell).status,
            CommandStatus::Completed { exit_code: 0 }
        );
        let second = shell.submit(
            "printf '%s\\n' \"$PWD\" \"$value\"; /bin/bash -c 'printf \"%s\\n\" \"$EXPORTED\"'; persistent_function; read ignored; printf '\\nstdin=%s' \"$?\"".into(),
            Duration::from_secs(5),
        )?;
        assert!(second > first);
        let command = completed(&shell);
        assert_eq!(command.id, second);
        assert_eq!(command.status, CommandStatus::Completed { exit_code: 0 });
        assert_eq!(
            command.output,
            format!(
                "{}/nested\nhello world\nenvironment\nfunction\nstdin=1",
                directory.path().canonicalize()?.display()
            )
        );
        shell.close()?;
        Ok(())
    }

    #[test]
    fn nonzero_is_not_session_failure_and_output_is_not_protocol() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(
            "printf '1 0\\n'; printf error >&2; false".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        assert_eq!(command.status, CommandStatus::Completed { exit_code: 1 });
        assert_eq!(command.output, "1 0\nerror");
        shell.submit("printf recovered".into(), Duration::from_secs(5))?;
        assert_eq!(completed(&shell).output, "recovered");
        shell.close()?;
        Ok(())
    }

    #[test]
    fn successful_completion_kills_and_reaps_background_jobs_and_preserves_status() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        for exit_code in [0, 7] {
            shell.submit(
                format!("/bin/sleep 60 & printf '%s\\n' \"$!\"; (exit {exit_code})"),
                Duration::from_secs(5),
            )?;
            let command = completed(&shell);
            assert_eq!(command.status, CommandStatus::Completed { exit_code });
            let child_pid: i32 = command.output.trim().parse()?;
            // wait must have reaped the job before the completion record, not merely
            // signalled it: even a zombie would still be visible to kill(pid, 0).
            assert!(
                !macos::process_exists(child_pid),
                "job {child_pid} was not reaped"
            );
            shell.submit(
                "builtin jobs -p; printf clean".into(),
                Duration::from_secs(5),
            )?;
            assert_eq!(completed(&shell).output, "clean");
        }
        shell.close()?;
        Ok(())
    }

    #[test]
    fn background_late_output_cannot_leak_into_the_next_command() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(
            "( while [[ ! -e release ]]; do /bin/sleep 0.01; done; printf late-stdout; printf late-stderr >&2 ) & printf '%s\\n' \"$!\"".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        assert_eq!(command.status, CommandStatus::Completed { exit_code: 0 });
        let child_pid: i32 = command.output.trim().parse()?;
        assert!(!macos::process_exists(child_pid));
        shell.submit(
            ": > release; /bin/sleep 0.1; printf next-command".into(),
            Duration::from_secs(5),
        )?;
        assert_eq!(completed(&shell).output, "next-command");
        shell.close()?;
        Ok(())
    }

    #[test]
    fn background_pipeline_members_are_all_killed_and_reaped() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(
            "/bin/sleep 60 | /bin/sleep 60 & builtin jobs -l".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        assert_eq!(command.status, CommandStatus::Completed { exit_code: 0 });
        let mut lines = command.output.lines();
        let leader_pid: i32 = lines
            .next()
            .expect("pipeline leader")
            .split_whitespace()
            .nth(1)
            .expect("leader pid")
            .parse()?;
        let other_pid: i32 = lines
            .next()
            .expect("pipeline member")
            .split_whitespace()
            .next()
            .expect("member pid")
            .parse()?;
        assert!(!macos::process_exists(leader_pid));
        assert!(!macos::process_exists(other_pid));
        shell.close()?;
        Ok(())
    }

    #[test]
    fn malicious_control_noise_fails_closed_and_cleans_up_the_group() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        let descriptor = shell.inner.control_descriptor;
        // The descriptor is deliberately accessible to evaluated code; rejecting this
        // malformed record is a lifecycle check, not authentication or a security boundary.
        shell.submit(
            format!("printf '%s\\n' \"$$\"; /bin/sleep 60 & printf '%s\\n' \"$!\"; builtin printf 'malicious-noise\\n' >&{descriptor}; wait"),
            Duration::from_secs(5),
        )?;
        let snapshot = wait_for(&shell, |snapshot| {
            matches!(snapshot.status, ShellStatus::Failed { .. })
        });
        assert!(
            matches!(&snapshot.status, ShellStatus::Failed { reason } if reason.contains("completion record"))
        );
        assert!(matches!(
            snapshot.command.expect("failed command").status,
            CommandStatus::Failed { .. }
        ));
        shell.close()?;
        let (shell_pid, child_pid) = child_ids(&shell);
        wait_gone(shell_pid);
        wait_gone(child_pid);
        wait_gone(-shell_pid);
        assert!(shell.submit("true".into(), Duration::from_secs(1)).is_err());
        Ok(())
    }

    #[test]
    fn submission_is_async_rejects_overlap_and_streams_output() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        let id = shell.submit(
            "printf started; while [[ ! -e release ]]; do /bin/sleep 0.01; done; printf finished"
                .into(),
            Duration::from_secs(10),
        )?;
        assert!(shell.submit("true".into(), Duration::from_secs(1)).is_err());
        let running = wait_for(&shell, |snapshot| {
            snapshot
                .command
                .as_ref()
                .is_some_and(|command| command.output == "started")
        });
        assert_eq!(running.status, ShellStatus::Running);
        assert_eq!(running.command.expect("running command").id, id);
        fs::write(directory.path().join("release"), "")?;
        assert_eq!(completed(&shell).output, "startedfinished");
        shell.close()?;
        assert_eq!(shell.snapshot().status, ShellStatus::Closed);
        assert!(shell.submit("true".into(), Duration::from_secs(1)).is_err());
        shell.close()?;
        Ok(())
    }

    #[test]
    fn timeout_kills_process_group_and_fails_closed() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(CHILD_COMMAND.into(), Duration::from_secs(1))?;
        let (shell_pid, child_pid) = child_ids(&shell);
        let snapshot = wait_for(&shell, |snapshot| {
            matches!(snapshot.status, ShellStatus::Failed { .. })
        });
        assert_eq!(
            snapshot.command.expect("timed out command").status,
            CommandStatus::TimedOut
        );
        shell.close()?;
        wait_gone(shell_pid);
        wait_gone(child_pid);
        wait_gone(-shell_pid);
        assert!(shell.submit("true".into(), Duration::from_secs(1)).is_err());
        Ok(())
    }

    #[test]
    fn drop_kills_process_group_and_is_bounded() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(CHILD_COMMAND.into(), Duration::from_secs(60))?;
        let (shell_pid, child_pid) = child_ids(&shell);
        let start = Instant::now();
        drop(shell);
        assert!(start.elapsed() < Duration::from_secs(3));
        wait_gone(shell_pid);
        wait_gone(child_pid);
        wait_gone(-shell_pid);
        Ok(())
    }

    #[test]
    fn close_cancels_running_command_and_kills_process_group() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(CHILD_COMMAND.into(), Duration::from_secs(60))?;
        let (shell_pid, child_pid) = child_ids(&shell);
        shell.close()?;
        let snapshot = shell.snapshot();
        assert_eq!(snapshot.status, ShellStatus::Closed);
        assert_eq!(
            snapshot.command.expect("cancelled command").status,
            CommandStatus::Cancelled
        );
        wait_gone(shell_pid);
        wait_gone(child_pid);
        wait_gone(-shell_pid);
        Ok(())
    }

    #[test]
    fn unexpected_exit_and_exec_fail_closed_and_clean_up_children() -> Result<()> {
        for ending in [
            "exit 7",
            "exec /usr/bin/true",
            "exec /nonexistent/persistent-shell-test",
        ] {
            let directory = TempDir::new()?;
            let shell = PersistentShell::spawn(directory.path().into()).context("spawn")?;
            shell
                .submit(
                    format!(
                        "printf '%s\\n' \"$$\"; /bin/sleep 60 & printf '%s\\n' \"$!\"; {ending}"
                    ),
                    Duration::from_secs(5),
                )
                .context("submit")?;
            let snapshot = wait_for(&shell, |snapshot| {
                matches!(snapshot.status, ShellStatus::Failed { .. })
            });
            assert!(matches!(
                snapshot.command.as_ref().expect("failed command").status,
                CommandStatus::Failed { .. }
            ));
            shell.close().context("close")?;
            // Exit may be observed before stdout was read. The worker must retain it.
            let (shell_pid, child_pid) = child_ids(&shell);
            wait_gone(shell_pid);
            wait_gone(child_pid);
            wait_gone(-shell_pid);
            assert!(shell.submit("true".into(), Duration::from_secs(1)).is_err());
        }
        Ok(())
    }

    #[test]
    fn output_is_bounded_without_blocking_producers() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(
            "/usr/bin/yes x | /usr/bin/head -c 1048576".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        assert_eq!(command.status, CommandStatus::Completed { exit_code: 0 });
        assert_eq!(command.output.len(), OUTPUT_LIMIT);
        assert!(command.output_truncated);
        shell.submit("printf clean".into(), Duration::from_secs(5))?;
        let command = completed(&shell);
        assert_eq!(command.output, "clean");
        assert!(!command.output_truncated);
        shell.close()?;
        Ok(())
    }

    #[test]
    fn output_flood_cannot_starve_timeout_or_cleanup() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit(
            "printf '%s\\n' \"$$\"; exec /usr/bin/yes".into(),
            Duration::from_millis(500),
        )?;
        let snapshot = wait_for(&shell, |snapshot| {
            matches!(snapshot.status, ShellStatus::Failed { .. })
        });
        shell.close()?;
        let command = snapshot.command.expect("flood command");
        assert_eq!(command.status, CommandStatus::TimedOut);
        assert_eq!(command.output.len(), OUTPUT_LIMIT);
        assert!(command.output_truncated);
        let shell_pid: i32 = command.output.lines().next().expect("shell pid").parse()?;
        wait_gone(shell_pid);
        wait_gone(-shell_pid);
        Ok(())
    }

    #[test]
    fn close_cleans_up_background_grandchildren_after_completion() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        // The nested shell prints the grandchild pid before exiting. The orphaned
        // sleep still belongs to our original shell's process group after completion.
        shell.submit(
            "printf '%s\\n' \"$$\"; /bin/bash -c '/bin/sleep 60 & printf \"%s\\n\" \"$!\"'".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        let mut lines = command.output.lines();
        let shell_pid: i32 = lines.next().expect("shell pid").parse()?;
        let grandchild_pid: i32 = lines.next().expect("grandchild pid").parse()?;
        assert!(macos::process_exists(grandchild_pid));
        shell.close()?;
        wait_gone(shell_pid);
        wait_gone(grandchild_pid);
        wait_gone(-shell_pid);
        Ok(())
    }

    #[test]
    fn snapshots_are_serializable_and_invalid_utf8_stays_bounded() -> Result<()> {
        let directory = TempDir::new()?;
        let shell = PersistentShell::spawn(directory.path().into())?;
        shell.submit("printf '\\377\\000text'".into(), Duration::from_secs(5))?;
        let command = completed(&shell);
        assert_eq!(command.output, "\u{fffd}\0text");
        let encoded = serde_json::to_string(&shell.snapshot())?;
        let decoded: ShellSnapshot = serde_json::from_str(&encoded)?;
        assert_eq!(decoded.status, ShellStatus::Idle);
        assert_eq!(
            decoded.command.expect("decoded command").output,
            command.output
        );
        shell.submit(
            "/bin/dd if=/dev/zero bs=65536 count=1 2>/dev/null | LC_ALL=C /usr/bin/tr '\\000' '\\377'".into(),
            Duration::from_secs(5),
        )?;
        let command = completed(&shell);
        assert!(command.output.len() <= OUTPUT_LIMIT);
        assert!(
            command
                .output
                .chars()
                .all(|character| character == '\u{fffd}')
        );
        assert!(command.output_truncated);
        shell.close()?;
        Ok(())
    }

    #[test]
    fn invalid_input_and_spawn_failure_do_not_start_work() -> Result<()> {
        let directory = TempDir::new()?;
        assert!(PersistentShell::spawn(directory.path().join("does-not-exist")).is_err());
        let shell = PersistentShell::spawn(directory.path().into())?;
        assert!(shell.submit("true".into(), Duration::ZERO).is_err());
        assert!(
            shell
                .submit("true\0false".into(), Duration::from_secs(1))
                .is_err()
        );
        assert!(
            shell
                .submit("x".repeat(COMMAND_LIMIT + 1), Duration::from_secs(1))
                .is_err()
        );
        assert_eq!(shell.snapshot().status, ShellStatus::Idle);
        assert!(shell.snapshot().command.is_none());
        shell.close()?;
        Ok(())
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn rejects_unsupported_platform() {
        assert!(PersistentShell::spawn(PathBuf::from(".")).is_err());
    }
}
