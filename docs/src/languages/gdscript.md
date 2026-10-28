---
title: GDScript
description: "Configure GDScript language support in Lin, including language servers, formatting, and debugging."
---

# GDScript

Godot [GDScript](https://gdscript.com/) language support in Lin is provided by the community-maintained [GDScript extension](https://github.com/GDQuest/lin-gdscript).
Report issues to: [https://github.com/GDQuest/lin-gdscript/issues](https://github.com/GDQuest/lin-gdscript/issues)

- Tree-sitter: [PrestonKnopp/tree-sitter-gdscript](https://github.com/PrestonKnopp/tree-sitter-gdscript) and [PrestonKnopp/tree-sitter-godot-resource](https://github.com/PrestonKnopp/tree-sitter-godot-resource)
- Language Server: [gdscript-language-server](https://github.com/godotengine/godot)

## Pre-requisites

You will need:

- [Godot](https://godotengine.org/download/).
- netcat (`nc` or `ncat`) on your system PATH.

## Setup

1. Inside your Godot editor, open Editor Settings, look for `Text Editor -> External` and set the following options:
   - Exec Path: `/path/to/lin`
   - Exec Flags: `{project} {file}:{line}:{col}`
   - Use External Editor: "✅ On"
2. Open any \*.gd file through Godot and Lin will launch.

## Usage

When Godot is running, the GDScript extension will connect to the language server provided by the Godot runtime and will provide `jump to definition`, hover states when you hold Ctrl/cmd and other language server features.
