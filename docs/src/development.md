---
title: Developing Lin
description: "Guide to building and developing Lin from source."
---

# Developing Lin

See the platform-specific instructions for building Lin from source:

- [macOS](./development/macos.md)
- [Linux](./development/linux.md)
- [Windows](./development/windows.md)

## Bird's-eye view of Lin {#birds-eye-view-of-lin}

Keep the [Lin glossary](./development/glossary.md) handy when starting out. It explains structures and terms you'll see throughout the codebase.

Lin is made up of several smaller crates. Here are the ones you're most likely to work with:

- [`gpui`](https://github.com/lin-editor/lin/tree/main/crates/gpui) is a GPU-accelerated UI framework which provides all of the building blocks for Lin. **We recommend familiarizing yourself with the root level GPUI documentation.**
- [`editor`](https://github.com/lin-editor/lin/tree/main/crates/editor) contains the core `Editor` type that drives both the code editor and all various input fields within Lin. It also handles a display layer for LSP features such as Inlay Hints or code completions.
- [`project`](https://github.com/lin-editor/lin/tree/main/crates/project) manages files and navigation within the filetree. It is also Lin's side of communication with LSP.
- [`workspace`](https://github.com/lin-editor/lin/tree/main/crates/workspace) handles local state serialization and groups projects together.
- [`vim`](https://github.com/lin-editor/lin/tree/main/crates/vim) is a thin implementation of Vim workflow over `editor`.
- [`lsp`](https://github.com/lin-editor/lin/tree/main/crates/lsp) handles communication with external LSP server.
- [`language`](https://github.com/lin-editor/lin/tree/main/crates/language) drives `editor`'s understanding of language - from providing a list of symbols to the syntax map.
- [`collab`](https://github.com/lin-editor/lin/tree/main/crates/collab) is the collaboration server itself, driving the collaboration features such as project sharing.
- [`rpc`](https://github.com/lin-editor/lin/tree/main/crates/rpc) defines messages to be exchanged with collaboration server.
- [`theme`](https://github.com/lin-editor/lin/tree/main/crates/theme) defines the theme system and provides a default theme.
- [`ui`](https://github.com/lin-editor/lin/tree/main/crates/ui) is a collection of UI components and common patterns used throughout Lin.
- [`cli`](https://github.com/lin-editor/lin/tree/main/crates/cli) is the CLI crate which invokes the Lin binary.
- [`lin`](https://github.com/lin-editor/lin/tree/main/crates/lin) is where all things come together, and the `main` entry point for Lin.

## Keychain access

Lin stores secrets in the system keychain.

However, when running a development build of Lin on macOS (and perhaps other
platforms) trying to access the keychain results in a lot of keychain prompts
that require entering your password over and over.

On macOS this is caused by the development build not having a stable identity.
Even if you choose the "Always Allow" option, the OS will still prompt you for
your password again the next time something changes in the binary.

This quickly becomes annoying and impedes development speed.

That is why, by default, when running a development build of Lin an alternative
credential provider is used to bypass the system keychain.

> **Note:** This is **only** the case for development builds. For all non-development
> release channels the system keychain is always used.

If you need to test something out using the real system keychain in a
development build, run Lin with the following environment variable set:

```
ZED_DEVELOPMENT_USE_KEYCHAIN=1
```

## Performance Measurements

Lin includes a frame time measurement system that can be used to profile how long it takes to render each frame. This is particularly useful when comparing rendering performance between different versions or when optimizing frame rendering code.

### Using ZED_MEASUREMENTS

To enable performance measurements, set the `ZED_MEASUREMENTS` environment variable:

```sh
export ZED_MEASUREMENTS=1
```

When enabled, Lin will print frame rendering timing information to stderr, showing how long each frame takes to render.

### Performance Comparison Workflow

Here's a typical workflow for comparing frame rendering performance between different versions:

1. **Enable measurements:**

   ```sh
   export ZED_MEASUREMENTS=1
   ```

2. **Test the first version:**

   - Checkout the commit you want to measure
   - Run Lin in release mode and use it for 5-10 seconds: `cargo run --release &> version-a`

3. **Test the second version:**

   - Checkout another commit you want to compare
   - Run Lin in release mode and use it for 5-10 seconds: `cargo run --release &> version-b`

4. **Generate comparison:**

   ```sh
   script/histogram version-a version-b
   ```

The `script/histogram` tool can accept as many measurement files as you like and will generate a histogram visualization comparing the frame rendering performance data between the provided versions.

### Using `util_macros::perf`

For benchmarking unit tests, annotate them with the `#[perf]` attribute from the `util_macros` crate. Then run `cargo
perf-test -p $CRATE` to benchmark them. See the rustdoc documentation on `crates/util_macros` and `tooling/perf` for
in-depth examples and explanations.

## ETW Profiling on Windows

Lin supports performance profiling with Event Tracing for Windows (ETW) to capture detailed performance data, including CPU, GPU, memory, disk, and file I/O activity. Data is saved to an `.etl` file, which can be opened in standard profiling tools for analysis.

ETW recordings may contain personally identifiable or security-sensitive information, such as paths to files and registry keys accessed, as well as process names. Please keep this in mind when sharing traces with others.

### Recording a trace

Open the command palette and run one of the following:

- `lin: record etw trace`: records CPU, GPU, memory, and I/O activity
- `lin: record etw trace with heap tracing`: includes heap allocation data for the Lin process

Lin will request administrator permission. Once granted, recording will begin.

### Saving or canceling

While a trace is recording, open the command palette and run one of the following:

- `lin: save etw trace`: stops recording and saves the trace to disk
- `lin: cancel etw trace`: stops recording without saving

## Contributor links

- [CONTRIBUTING.md](https://github.com/lin-editor/lin/blob/main/CONTRIBUTING.md)
- [Debugging Crashes](./development/debugging-crashes.md)
- [Code of Conduct](https://lin.dev/code-of-conduct)
- [Lin Contributor License](https://lin.dev/cla)
