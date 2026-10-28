---
title: CLI Reference
description: "Reference for Lin's command-line interface (CLI), including opening files and directories, integrating with tools, and controlling Lin from scripts."
---

# CLI Reference

Use Lin's command-line interface (CLI) to open files and directories, integrate with other tools, and control Lin from scripts.

## Installation

**macOS:** Run the {#action cli::InstallCliBinary} command from the command palette ({#kb command_palette::Toggle}) to install the `lin` CLI to `/usr/local/bin/lin`.

**Linux:** The CLI is included with Lin packages and is named `lin`.

**Windows:** The CLI is included with Lin. Add Lin's installation directory to your PATH, or use the full path to `lin.exe`.

## Usage

```sh
lin [OPTIONS] [PATHS]...
```

## Opening Files and Directories

Open a file:

```sh
lin myfile.txt
```

Open a directory as a workspace:

```sh
lin ~/projects/myproject
```

Open multiple files or directories:

```sh
lin file1.txt file2.txt ~/projects/myproject
```

Open a file at a specific line and column:

```sh
lin myfile.txt:42        # Open at line 42
lin myfile.txt:42:10     # Open at line 42, column 10
```

## Options

### `-w`, `--wait`

Wait for all opened files to be closed before the CLI exits. When opening a directory, waits until the window is closed.

This is useful for integrating Lin with tools that expect an editor to block until editing is complete (e.g., `git commit`):

```sh
export EDITOR="lin --wait"
git commit  # Opens Lin and waits for you to close the commit message file
```

### `-n`, `--new`

Open paths in a new workspace window, even if the paths are already open in an existing window:

```sh
lin -n ~/projects/myproject
```

### `-a`, `--add`

Add paths to the currently focused workspace instead of opening a new window. When multiple workspace windows are open, files open in the focused window:

```sh
lin -a newfile.txt
```

### `-r`, `--reuse`

Reuse an existing window, replacing its current workspace with the new paths:

```sh
lin -r ~/projects/different-project
```

### `-e`, `--existing`

Open paths in an existing Lin window instead of creating a new one:

```sh
lin -e myfile.txt
```

By default (without `-n`, `-a`, `-r`, or `-e`), directories open in the current window's sidebar. You can change this default with the `cli_default_open_behavior` setting. See [Windows & Projects](../windows-and-projects.md) for more details.

### `--diff <OLD_PATH> <NEW_PATH>`

Open a diff view comparing two files. Can be specified multiple times:

```sh
lin --diff file1.txt file2.txt
lin --diff old.rs new.rs --diff old2.rs new2.rs
```

### `--foreground`

Run Lin in the foreground, keeping the terminal attached. Useful for debugging:

```sh
lin --foreground
```

### `--user-data-dir <DIR>`

Use a custom directory for all user data (database, extensions, logs) instead of the default location:

```sh
lin --user-data-dir ~/.lin-custom
```

Default locations:

- **macOS:** `~/Library/Application Support/Lin`
- **Linux:** `$XDG_DATA_HOME/lin` (typically `~/.local/share/lin`)
- **Windows:** `%LOCALAPPDATA%\Lin`

### `-v`, `--version`

Print Lin's version and exit:

```sh
lin --version
```

### `--completions <SHELL>`

Generate shell completions for the `lin` CLI:

#### Bash

Add to `~/.bashrc`:

```bash
eval "$(lin --completions bash)"
```

#### Elvish

Add to `~/.config/elvish/rc.elv`:

```elvish
set edit:completion:arg-completer[lin] = { |@args|
    eval (lin --completions elvish | slurp)
    $edit:completion:arg-completer[lin] $@args
}
```

#### Fish

Add to `~/.config/fish/config.fish`:

```fish
lin --completions fish | source
```

#### Nushell

Add to `~/.config/nushell/config.nu`:

```nu
mkdir ($nu.data-dir | path join "vendor/autoload")
^lin --completions nushell | save --force ($nu.data-dir | path join "vendor/autoload/lin.nu")
```

#### Powershell

Add to `$PROFILE`:

```powershell
(&lin --completions powershell) | Out-String | Invoke-Expression
```

#### Zsh

Add to `~/.zshrc`:

```zsh
eval "$(lin --completions zsh)"
```

### `--uninstall`

Uninstall Lin and remove all related files (macOS and Linux only):

```sh
lin --uninstall
```

### `--lin <PATH>`

Specify a custom path to the Lin application or binary:

```sh
lin --lin /path/to/Lin.app myfile.txt
```

## Reading from Standard Input

Read content from stdin by passing `-` as the path:

```sh
echo "Hello, World!" | lin -
cat myfile.txt | lin -
ps aux | lin -
```

This creates a temporary file with the stdin content and opens it in Lin.

## URL Handling

The CLI can open `lin://`, `file://`, and `ssh://` URLs:

```sh
lin lin://settings
lin file:///Users/whatever/.zshrc
lin ssh://me@example.com/abs/path
lin ssh://me@example.com:/abs/path
lin ssh://me@example.com/~/project
lin ssh://me@example.com:~/project
```

## Using Lin as Your Default Editor

Set Lin as your default editor for Git and other tools:

```sh
export EDITOR="lin --wait"
export VISUAL="lin --wait"
```

Add these lines to your shell configuration file (e.g., `~/.bashrc`, `~/.zshrc`).

## macOS: Switching Release Channels

On macOS, you can launch a specific release channel by passing the channel name as the first argument:

```sh
lin --stable myfile.txt
lin --preview myfile.txt
lin --nightly myfile.txt
```

## WSL Integration (Windows)

On Windows, the CLI supports opening paths from WSL distributions. This is handled automatically when launching Lin from within WSL.

## Exit Codes

| Code | Meaning                           |
| ---- | --------------------------------- |
| `0`  | Success                           |
| `1`  | Error (details printed to stderr) |

When using `--wait`, the exit code reflects whether the files were saved before closing.
