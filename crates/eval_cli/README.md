# eval-cli

Headless Rust binary for running Zed's agent in evaluation and benchmark
environments. It is designed for containerized harnesses such as
[Harbor](https://harborframework.com/) and Pier, where the repository is already
checked out and model API keys are provided via environment variables.

`eval-cli` uses the same `NativeAgent` + `AcpThread` pipeline as the production
Zed editor: a full agentic loop with tool calls, subagents, and retries, without
a GUI.

This directory also contains `zed_eval/`, the Python `zed-eval` package used to
build this binary, launch remote benchmark runs on Modal/Harbor/Pier, and fetch
results. For normal benchmark orchestration, start with
[`zed_eval/README.md`](zed_eval/README.md).

## Building

### Native, for local testing on the same OS

```sh
cargo build --release -p eval_cli
```

### Linux x86_64, for Harbor/Pier sandboxes

Harbor and Pier containers run Linux x86_64. From the repository root, use the
Docker-based build script:

```sh
crates/eval_cli/script/build-linux
```

This produces `target/eval-cli`, an x86_64 Linux ELF binary. You can also
specify a custom output path:

```sh
crates/eval_cli/script/build-linux --output ~/bin/eval-cli-linux
```

## Standalone usage

```sh
eval-cli \
  --workdir /testbed \
  --model anthropic/claude-sonnet-4-6 \
  --instruction "Fix the bug described in..." \
  --timeout 600 \
  --output-dir /logs/agent
```

`eval-cli` reads provider API keys from environment variables such as
`ANTHROPIC_API_KEY` and `OPENAI_API_KEY`. It writes `result.json`, `thread.md`,
and `thread.json` to the output directory.

### Isolated read-only evaluation

`--isolated` is an opt-in path for `openai-subscribed/gpt-6.1-terra`. Set
`ZED_STATELESS=1` in the environment before starting the process. Supply a
separate fixture directory containing only regular copies of `permission_tier.rs`
and/or `sandboxing.rs` (no hidden entries or symlinks), and an existing output
directory outside the fixture. The isolated profile exposes only `read_file`, does not
start project context servers, and does not add the output directory as a
worktree. Normal eval behavior is unchanged.

This is **not** an OS sandbox or a network egress policy. The fixture check is
not race-resistant, `read_file` also supports the global skills directory, and
the process can access its ambient environment and filesystem. The subscribed
provider reads its stored credentials and may refresh tokens and fetch its model
catalog. The application also constructs other services and an HTTP client;
proxy environment variables can affect traffic. Artifacts contain conversation
content. Before any real run, use a separately controlled fixture and process
identity/environment, restrict filesystem access and outbound destinations at
the OS or network layer, and verify those restrictions without revealing tokens.
Do not run online if those restrictions cannot be established. Preserve only
the target provider credentials needed for the run, and protect the output.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Agent finished |
| 1 | Error, such as model/auth/runtime failure |
| 2 | Timeout |
| 3 | Interrupted by SIGTERM or SIGINT |

## Running benchmarks

Most benchmark runs should use the Python `zed-eval` CLI instead of invoking
`eval-cli` directly. From the repository root:

```sh
crates/eval_cli/script/install-zed-eval
zed-eval doctor --create-volume
zed-eval run rf --from local --n-tasks 2
```

For one-off source runs without installing the tool globally, use
`crates/eval_cli/script/zed-eval <args>`.

See [`zed_eval/README.md`](zed_eval/README.md) for supported benchmarks, remote
builds, Modal setup, reporting, rejudging, baselines, and Harbor/Pier installed
agent usage.
