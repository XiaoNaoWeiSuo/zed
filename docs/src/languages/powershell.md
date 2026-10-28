---
title: PowerShell
description: "Configure PowerShell language support in Lin, including language servers, formatting, and debugging."
---

# PowerShell

PowerShell language support in Lin is provided by the community-maintained [Lin PowerShell extension](https://github.com/wingyplus/lin-powershell). Please report issues to: [github.com/wingyplus/lin-powershell/issues](https://github.com/wingyplus/lin-powershell/issues)

- Tree-sitter: [airbus-cert/tree-sitter-powershell](https://github.com/airbus-cert/tree-sitter-powershell)
- Language Server: [PowerShell/PowerShellEditorServices](https://github.com/PowerShell/PowerShellEditorServices)

## Setup

### Install PowerShell 7+ {#powershell-install}

- macOS: `brew install powershell/tap/powershell`
- Alpine: [Installing PowerShell on Alpine Linux](https://learn.microsoft.com/en-us/powershell/scripting/install/install-alpine)
- Debian: [Install PowerShell on Debian Linux](https://learn.microsoft.com/en-us/powershell/scripting/install/install-debian)
- RedHat: [Install PowerShell on RHEL](https://learn.microsoft.com/en-us/powershell/scripting/install/install-rhel)
- Ubuntu: [Install PowerShell on RHEL](https://learn.microsoft.com/en-us/powershell/scripting/install/install-ubuntu)
- Windows: [Install PowerShell on Windows](https://learn.microsoft.com/en-us/powershell/scripting/install/installing-powershell-on-windows)

The Lin PowerShell extension will default to the `pwsh` executable found in your path.

### Install PowerShell Editor Services (Optional) {#powershell-editor-services}

The Lin PowerShell extension will attempt to download [PowerShell Editor Services](https://github.com/PowerShell/PowerShellEditorServices) automatically.

If you want to use a specific binary, you can specify that in your Lin settings.json:

```json [settings]
  "lsp": {
    "powershell-es": {
      "binary": {
        "path": "/path/to/PowerShellEditorServices"
      }
    }
  }
```
