---
title: Developing Extensions
description: "Create Lin extensions: languages, themes, debuggers, and more."
---

# Developing Extensions {#developing-extensions}

Lin extensions are Git repositories containing an `extension.toml` manifest. They can provide languages, themes, debuggers, snippets, and MCP servers.

## Extension Features {#extension-features}

Extensions can provide:

- [Languages](./languages.md)
- [Debuggers](./debugger-extensions.md)
- [Themes](./themes.md)
- [Icon Themes](./icon-themes.md)
- [Snippets](./snippets.md)
- [MCP Servers](./mcp-extensions.md)

Lin automatically assigns your extension to categories on the Extensions page based on the features it provides. An extension that provides multiple features may appear in more than one category.

Note that published extensions must meet our [extension publishing prerequisites](./publishing/prerequisites.md), which include requirements on the features an extension provides.

## Developing an Extension Locally

Before starting to develop an extension for Lin, be sure to [install Rust via rustup](https://www.rust-lang.org/tools/install).

> Lin uses the `wasm32-wasip2` Rust target to compile extensions. If Rust is installed via rustup, Lin will install the target automatically. If Rust is installed another way (e.g., via Homebrew or Nix), you must make the `wasm32-wasip2` target available yourself — for example, by adding it to the `targets` of a Nix rust-overlay or fenix toolchain.

Extensions that provide grammars additionally require the [wasi-sdk](https://github.com/WebAssembly/wasi-sdk) to compile Tree-sitter parsers. Lin downloads it automatically, but you can point Lin at an existing installation by setting the `WASI_SDK_PATH` environment variable to its root directory (the one containing `bin/clang`).

When developing an extension, you can use it in Lin without needing to publish it by installing it as a _dev extension_.

From the extensions page, click the `Install Dev Extension` button (or the {#action lin::InstallDevExtension} action) and select the directory containing your extension.

If you need to troubleshoot, check Lin.log ({#action lin::OpenLog}) for additional output. For debug output, close and relaunch Lin from the command line with `lin --foreground`, which shows more verbose INFO-level logs.

If you already have the published version of the extension installed, the published version will be uninstalled prior to the installation of the dev extension. After successful installation, the `Extensions` page will indicate that the upstream extension is "Overridden by dev extension".

## Directory Structure of a Lin Extension

A Lin extension is a Git repository that contains an `extension.toml`. This file must contain some
basic information about the extension:

```toml
id = "my-extension"
name = "My extension"
version = "0.0.1"
schema_version = 1
authors = ["Your Name <you@example.com>"]
description = "Example extension"
repository = "https://github.com/your-name/my-lin-extension"
```

In addition to this, there are several other optional files and directories that can be used to add functionality to a Lin extension. An example directory structure of an extension that provides all capabilities is as follows:

```
my-extension/
  extension.toml
  Cargo.toml
  src/
    lib.rs
  languages/
    my-language/
      config.toml
      highlights.scm
  themes/
    my-theme.json
  snippets/
    snippets.json
    rust.json
```

## Rust and WebAssembly

> Please note that most extensions will work properly without any Rust code present. In particular, only language server, context server and debugger extensions require the presence of custom Rust in order to function properly.

Procedural parts of extensions are written in Rust and compiled to WebAssembly. To develop an extension that includes custom code, include a `Cargo.toml` like this:

```toml
[package]
name = "my-extension"
version = "0.0.1"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
lin_extension_api = "0.1.0"
```

Use the latest version of the [`lin_extension_api`](https://crates.io/crates/lin_extension_api) available on crates.io. Make sure it's still [compatible with Lin versions](https://github.com/lin-editor/lin/blob/main/crates/extension_api#compatible-lin-versions) you want to support.

In the `src/lib.rs` file in your Rust crate you will need to define a struct for your extension and implement the `Extension` trait, as well as use the `register_extension!` macro to register your extension:

```rs
use lin_extension_api as lin;

struct MyExtension {
    // ... state
}

impl lin::Extension for MyExtension {
    // ...
}

lin::register_extension!(MyExtension);
```

> Since your extension will be compiled to WebAssembly, some Rust features might not work like you would expect them to. For example, `cfg` - directives will not work and `std::env::var` will also not yield the expected results. Instead, use the [`lin_extension_api::current_platform`](https://docs.rs/lin_extension_api/latest/lin_extension_api/fn.current_platform.html) method to get information about the current environment and familiarize yourself with the [`Worktree` struct and its methods](https://docs.rs/lin_extension_api/latest/lin_extension_api/struct.Worktree.html) for reading environment variables and finding binaries in the user's `PATH`.

### Debugging your Rust extension

`stdout`/`stderr` is forwarded directly to the Lin process. In order to see `println!`/`dbg!` output from your extension, you can start Lin in your terminal with a `--foreground` flag.
