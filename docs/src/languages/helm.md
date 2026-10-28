---
title: Helm
description: "Configure Helm language support in Lin, including language servers, formatting, and debugging."
---

# Helm

Support for Helm in Lin is provided by the community-maintained [Helm extension](https://github.com/cabrinha/helm.lin).

- Tree-sitter: [tree-sitter-go-template](https://github.com/ngalaiko/tree-sitter-go-template/tree/master)
- Language Server: [mrjosh/helm-ls](https://github.com/mrjosh/helm-ls)

## Setup

Enable Helm language for Helm files by editing your `.lin/settings.json` and adding:

```json [settings]
  "file_types": {
    "Helm": [
      "**/templates/**/*.tpl",
      "**/templates/**/*.yaml",
      "**/templates/**/*.yml",
      "**/helmfile.d/**/*.yaml",
      "**/helmfile.d/**/*.yml",
      "**/values*.yaml"
    ]
  }
```

This will also mark values.yaml files as the type helm, since helm-ls supports this.
