---
title: Building Lin for Linux
description: "Guide to building lin for linux for Lin development."
---

# Building Lin for Linux

## Repository

Clone the [Lin repository](https://github.com/lin-editor/lin).

## Dependencies

- Install [rustup](https://www.rust-lang.org/tools/install)

- Install the necessary system libraries:

  ```sh
  script/linux
  ```

  If you prefer to install the system libraries manually, you can find the list of required packages in the `script/linux` file.

## Building from source

Once the dependencies are installed, you can build Lin using [Cargo](https://doc.rust-lang.org/cargo/).

For a debug build of the editor:

```sh
cargo run
```

And to run the tests:

```sh
cargo test --workspace
```

In release mode, the primary user interface is the `cli` crate. You can run it in development with:

```sh
cargo run -p cli
```

## Installing a development build

You can install a local build on your machine with:

```sh
./script/install-linux
```

This builds `lin` and the `cli` in release mode, installs the binary at `~/.local/bin/lin`, and installs `.desktop` files to `~/.local/share`.

## Wayland & X11

Lin supports both X11 and Wayland. By default, we pick whichever we can find at runtime. If you're on Wayland and want to run in X11 mode, use the environment variable `WAYLAND_DISPLAY=''`.

## Notes for packaging Lin

### Technical requirements

Lin has two main binaries:

- Build `crates/cli` and make its binary available in `$PATH` with the name `lin`.
- Build `crates/lin` and put it at `$PATH/to/cli/../../libexec/lin-editor`. For example, if the CLI is at `~/.local/bin/lin`, put the editor at `~/.local/libexec/lin-editor`. Distributions that discourage `libexec` can use `$PATH/to/cli/../../lib/lin/lin-editor` instead.
- A `.desktop` template lives at `crates/lin/resources/lin.desktop.in`; populate it with `envsubst` and rename it to `$APP_ID.desktop`.
- Ensure the necessary libraries are installed; `script/bundle-linux` shows the expected set.
- `crates/lin/RELEASE_CHANNEL` must contain `nightly`, `preview`, or `stable` with no trailing newline.

### Other things to note

- Different Linux distributions refresh at different speeds; package from a tagged commit.
- Lin installs common developer tools on demand, similar to rustup/rbenv/pyenv.
- Extensions are installed from the official Lin extension registry.
- Lin does not contact hosted services. Telemetry, crash reporting, and automatic updates are disabled.

## Memory profiling

[`heaptrack`](https://github.com/KDE/heaptrack) is quite useful for diagnosing memory leaks. To install it:

```sh
$ sudo apt install heaptrack heaptrack-gui
$ cargo install cargo-heaptrack
```

Then, to build and run Lin with the profiler attached:

```sh
$ cargo heaptrack -b lin
```

When this lin instance is exited, terminal output will include a command to run `heaptrack_interpret` to convert the `*.raw.zst` profile to a `*.zst` file which can be passed to `heaptrack_gui` for viewing.

## Perf recording

How to get a flamegraph with resolved symbols from a running Lin instance.
Use this when Lin is using a lot of CPU. It is not useful for hangs.

### During the incident

- Find the PID (process ID) using:
  `ps -eo size,pid,comm | grep lin | sort | head -n 1 | cut -d ' ' -f 2`
  Or find the PID of `lin-editor` with the highest RAM usage in something
  like htop/btop/top.

- Install perf:
  On Ubuntu (derivatives) run `sudo apt install linux-tools`.

- Perf record:
  Run `sudo perf record -g --call-graph dwarf -p <pid you just found>`, wait a few seconds to gather data, then press Ctrl+C. You should now have a `perf.data` file.
  The `--call-graph dwarf` option records callers of every sampled function and unwinds through the `.eh_frame` data kept in stripped release binaries; plain `-g` does not work without frame pointers.

- Make the output file user owned:
  run `sudo chown $USER:$USER perf.data`

- Get build info:
  Run lin again and type {#action lin::About} in the command pallet to get the exact commit.

The `perf.data` file can be sent to Lin together with the exact commit.

### Later

This can be done by Lin staff.

Released Linux binaries are stripped, but the unstripped binary for every release is archived in Sentry (uploaded by `script/bundle-linux`).
Download it from the Sentry project's Debug Files page (search by the release version or the binary's build id), then continue with the `perf buildid-cache` step below.
Stripping preserves the build id, so the downloaded binary matches the user's `perf.data`.

Alternatively, rebuild the binary with symbols:

- Check out the commit found previously and modify `Cargo.toml`.
  Apply the following diff, then make a release build.

```diff
[profile.release]
-debug = "limited"
+debug = "full"
```

- Add the symbols to the perf database:
  `perf buildid-cache -v -a <path to release lin binary>`

- Resolve the symbols from the db:
  `perf inject -i perf.data -o perf_with_symbols.data`

- Install flamegraph:
  `cargo install cargo-flamegraph`

- Render the flamegraph:
  `flamegraph --perfdata perf_with_symbols.data`

## Troubleshooting

### Cargo errors claiming that a dependency is using unstable features

Try `cargo clean` and `cargo build`.
