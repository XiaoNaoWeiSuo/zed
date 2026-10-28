---
title: Install Lin - macOS, Linux, Windows
description: Download and install Lin on macOS, Linux, or Windows. Includes Homebrew, direct download, and package manager options.
---

# Installing Lin

## Download Lin

### macOS

Get the latest stable builds via [the download page](https://lin.dev/download). If you want to download our preview build, you can find it on its [releases page](https://lin.dev/releases/preview). After the first manual installation, Lin will periodically check for install updates.

You can also install Lin stable via Homebrew:

```sh
brew install --cask lin
```

As well as Lin preview:

```sh
brew install --cask lin@preview
```

### Windows

Get the latest stable builds via [the download page](https://lin.dev/download). If you want to download our preview build, you can find it on its [releases page](https://lin.dev/releases/preview). After the first manual installation, Lin will periodically check for install updates.

Additionally, you can install Lin using winget:

```sh
winget install -e --id LinIndustries.Lin
```

### Linux

For most Linux users, the easiest way to install Lin is through our installation script:

```sh
curl -f https://lin.dev/install.sh | sh
```

You can now optionally specify a **version** of Lin to install using the `ZED_VERSION` environment variable:

```sh
# Install the latest stable version (default)
curl -f https://lin.dev/install.sh | sh

# Install a specific version
curl -f https://lin.dev/install.sh | ZED_VERSION=0.216.0 sh
```

To install the preview build, which receives updates about a week ahead of stable:

```sh
curl -f https://lin.dev/install.sh | ZED_CHANNEL=preview sh
```

This script supports `x86_64` and `AArch64`, as well as common Linux distributions: Ubuntu, Arch, Debian, RedHat, CentOS, Fedora, and more.

If Lin is installed using this installation script, it can be uninstalled at any time by running the shell command `lin --uninstall`. The shell will then prompt you whether you'd like to keep your preferences or delete them. After making a choice, you should see a message that Lin was successfully uninstalled.

If this script is insufficient for your use case, you run into problems running Lin, or there are errors in uninstalling Lin, please see our [Linux-specific documentation](./linux.md).

## System Requirements

### macOS

Lin requires macOS 12.0 (Monterey) or later.

| Version            | Codename    | Apple Status   | Lin Status          |
| ------------------ | ----------- | -------------- | ------------------- |
| macOS 27.x         | Golden Gate | Supported      | Supported           |
| macOS 26.x         | Tahoe       | Supported      | Supported           |
| macOS 15.x         | Sequoia     | Supported      | Supported           |
| macOS 14.x         | Sonoma      | EOL 2026-09-14 | Supported           |
| macOS 13.x         | Ventura     | EOL 2025-09-15 | Supported           |
| macOS 12.x (12.3+) | Monterey    | EOL 2024-09-16 | Supported           |
| macOS 12.0–12.2    | Monterey    | EOL 2024-09-16 | Partially Supported |

The macOS releases labelled "Partially Supported" (Monterey 12.0–12.2) do not support screen sharing via Lin Collaboration.
Screen sharing uses [ScreenCaptureKit.framework](https://developer.apple.com/documentation/screencapturekit/), which is available on macOS 12.3 and newer.

Apple support status and EOL dates are from [endoflife.date](https://endoflife.date/macos).

#### Mac Hardware

Lin supports machines with Intel (x86_64) or Apple (aarch64) processors that meet the above macOS requirements:

- MacBook Pro (Early 2015 and newer)
- MacBook Air (Early 2015 and newer)
- MacBook (Early 2016 and newer)
- Mac Mini (Late 2014 and newer)
- Mac Pro (Late 2013 or newer)
- iMac (Late 2015 and newer)
- iMac Pro (all models)
- Mac Studio (all models)

### Linux

Lin supports 64-bit Intel/AMD (x86_64) and 64-bit Arm (aarch64) processors.

Lin requires a Vulkan 1.3 driver and the following desktop portals:

- `org.freedesktop.portal.FileChooser`
- `org.freedesktop.portal.OpenURI`
- `org.freedesktop.portal.Secret` or `org.freedesktop.Secrets`

### Windows

Lin supports the following Windows releases:
| Version | Lin Status |
| ------------------------- | ------------------- |
| Windows 11, version 22H2 and later | Supported |
| Windows 10, version 1903 and later | Supported |

A 64-bit operating system is required to run Lin.

#### Windows Hardware

Lin supports machines with x64 (Intel, AMD) or Arm64 (Qualcomm) processors that meet the following requirements:

- Graphics: A GPU that supports DirectX 11 (most PCs from 2012+).
- Driver: Current NVIDIA/AMD/Intel/Qualcomm driver (not the Microsoft Basic Display Adapter).

### FreeBSD

Not yet available as an official download. Can be built [from source](./development/freebsd.md).

### Web

Not supported at this time. See our [Platform Support issue](https://github.com/lin-editor/lin/issues/5391).
