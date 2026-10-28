---
title: Lin on Linux
description: "The installation script on the download page is the fastest way to install Lin:"
---

# Lin on Linux

## Standard Installation

The installation script on the [download](https://lin.dev/download) page is the fastest way to install Lin:

```sh
curl -f https://lin.dev/install.sh | sh
```

We also offer a preview build of Lin which receives updates about a week ahead of stable. You can install it with:

```sh
curl -f https://lin.dev/install.sh | ZED_CHANNEL=preview sh
```

The Lin installed by the script works best on systems that:

- have a Vulkan compatible GPU available (for example Linux on an M-series MacBook)
- have a system-wide glibc
  - x86_64 (Intel/AMD): glibc version >= 2.31 (Ubuntu 20 and newer)
  - aarch64 (ARM): glibc version >= 2.35 (Ubuntu 22 and newer)

NixOS does not have a system-wide glibc by default. If you'd like to use our builds on NixOS, they may work if you install a glibc compatibility layer such as [nix-ld](https://github.com/Mic92/nix-ld).

You will need to build from source for:

- architectures other than 64-bit Intel or 64-bit ARM (for example a 32-bit or RISC-V machine)
- Redhat Enterprise Linux 8.x, Rocky Linux 8, AlmaLinux 8, Amazon Linux 2 on all architectures
- Redhat Enterprise Linux 9.x, Rocky Linux 9.3, AlmaLinux 8, Amazon Linux 2023 on aarch64 (x86_x64 OK)

## Other ways to install Lin on Linux

Lin is open source, and [you can install from source](./development/linux.md).

### Installing via a package manager

There are several third-party Lin packages for various Linux distributions and package managers, sometimes under `lin-editor`. Availability varies by distribution, but you may be able to install Lin using one of these packages:

- Arch: [`lin`](https://archlinux.org/packages/extra/x86_64/lin/)
- Arch (AUR): [`lin-git`](https://aur.archlinux.org/packages/lin-git), [`lin-preview`](https://aur.archlinux.org/packages/lin-preview), [`lin-preview-bin`](https://aur.archlinux.org/packages/lin-preview-bin)
- Fedora/Ultramarine (Terra): [`lin`](https://github.com/terrapkg/packages/tree/frawhide/anda/devs/lin/stable), [`lin-preview`](https://github.com/terrapkg/packages/tree/frawhide/anda/devs/lin/preview), [`lin-nightly`](https://github.com/terrapkg/packages/tree/frawhide/anda/devs/lin/nightly)
- Homebrew: [`lin`](https://formulae.brew.sh/cask/lin), [`lin@preview`](https://formulae.brew.sh/cask/lin@preview)
- Manjaro: [`lin`](https://manjaristas.org/branch_compare?q=lin)
- Conda: [`lin`](https://anaconda.org/conda-forge/lin)
- Nix: `lin-editor` ([unstable](https://search.nixos.org/packages?channel=unstable&show=lin-editor))
- Solus: [`lin`](https://github.com/getsolus/packages/tree/main/packages/z/lin)
- Parabola: [`lin`](https://www.parabola.nu/packages/extra/x86_64/lin/)
- ALT Linux (Sisyphus): [`lin`](https://packages.altlinux.org/en/sisyphus/srpms/lin/)
- AOSC OS: [`lin`](https://packages.aosc.io/packages/lin)
- Flathub: [`dev.lin.Lin`](https://flathub.org/apps/dev.lin.Lin)

See [Repology](https://repology.org/project/lin-editor/versions) for a current list of Lin packages in various repositories.

### Community

When installing a third-party package please be aware that it may not be completely up to date and may be slightly different from the Lin we package (a common change is to rename the binary to `linit` or `linitor` to avoid conflicting with other packages).

We'd love your help making Lin available for everyone. If Lin is not yet available for your package manager, and you would like to fix that, we have some notes on [how to do it](./development/linux.md#notes-for-packaging-lin).

The packages in this section provide binary installs for Lin but are not official packages within the associated distributions. These packages are maintained by community members and as such a higher level of caution should be taken when installing them.

### Downloading manually

If you'd prefer, you can install Lin by downloading our pre-built .tar.gz. This is the same artifact that our install script uses, but you can customize the location of your installation by modifying the instructions below:

Download the `.tar.gz` file:

- [lin-linux-x86_64.tar.gz](https://cloud.lin.dev/releases/stable/latest/download?asset=lin&arch=x86_64&os=linux&source=docs)
  ([preview](https://cloud.lin.dev/releases/preview/latest/download?asset=lin&arch=x86_64&os=linux&source=docs))
- [lin-linux-aarch64.tar.gz](https://cloud.lin.dev/releases/stable/latest/download?asset=lin&arch=aarch64&os=linux&source=docs)
  ([preview](https://cloud.lin.dev/releases/preview/latest/download?asset=lin&arch=aarch64&os=linux&source=docs))

Then ensure that the `lin` binary in the tarball is on your path. The easiest way is to unpack the tarball and create a symlink:

```sh
mkdir -p ~/.local
# extract lin to ~/.local/lin.app/
tar -xvf <path/to/download>.tar.gz -C ~/.local
# link the lin binary to ~/.local/bin (or another directory in your $PATH)
ln -sf ~/.local/lin.app/bin/lin ~/.local/bin/lin
```

If you'd like integration with an XDG-compatible desktop environment, you will also need to install the `.desktop` file:

```sh
install -D ~/.local/lin.app/share/applications/dev.lin.Lin.desktop -t ~/.local/share/applications
sed -i "s|Icon=lin|Icon=$HOME/.local/lin.app/share/icons/hicolor/512x512/apps/lin.png|g" ~/.local/share/applications/dev.lin.Lin.desktop
sed -i "s|Exec=lin|Exec=$HOME/.local/lin.app/bin/lin|g" ~/.local/share/applications/dev.lin.Lin.desktop
```

## Uninstalling Lin

### Standard Uninstall

If Lin was installed using the default installation script, it can be uninstalled by supplying the `--uninstall` flag to the `lin` shell command:

```sh
lin --uninstall
```

Note that this will uninstall the Lin variant that the symlink points to. If you have multiple, parallel installations (such as both Stable and Preview), then you should instead use its absolute path as described further down in this section.

If there are no errors, the shell will then prompt you whether you'd like to keep your preferences or delete them. After making a choice, you should see a message that Lin was successfully uninstalled.

In the case that the `lin` shell command was not found in your PATH, you can try one of the following commands:

```sh
$HOME/.local/bin/lin --uninstall
```

or the absolute path to your installation, such as

```sh
$HOME/.local/lin.app/bin/lin --uninstall
```

The first case might fail if a symlink was not properly established between `$HOME/.local/bin/lin` and `$HOME/.local/lin.app/bin/lin`, or if the symlink was overwritten by having parallel installations of different versions of Lin. But the second command should always work as long as Lin was installed to its default location.

If Lin was installed to a different location (such as the `lin-preview.app` variant), you must invoke the `lin` binary stored in that installation directory and pass the `--uninstall` flag to it in the same format as the previous commands.

### Package Manager

If Lin was installed using a package manager, please consult the documentation for that package manager on how to uninstall a package.

## Troubleshooting

Linux works on a large variety of systems configured in many different ways. We primarily test Lin on a vanilla Ubuntu setup, as it is the most common distribution our users use. That said, we do expect it to work on a wide variety of machines.

### Lin fails to start

If you see an error like "/lib64/libc.so.6: version 'GLIBC_2.29' not found" it means that your distribution's version of glibc is too old. You can either upgrade your system, or [install Lin from source](./development/linux.md).

### Graphics issues

#### Lin fails to open windows

Lin requires a GPU to run effectively. Under the hood, we use [Vulkan](https://www.vulkan.org/) to communicate with your GPU. If you are seeing problems with performance, or Lin fails to load, it is possible that Vulkan is the culprit.

If you see a notification saying `Lin failed to open a window: NoSupportedDeviceFound` this means that Vulkan cannot find a compatible GPU. You can try running [vkcube](https://github.com/krh/vkcube) (usually available as part of the `vulkaninfo` or `vulkan-tools` package on various distributions) to try to troubleshoot where the issue is coming from like so:

```
vkcube
```

> **_Note_**: Try running in both X11 and wayland modes by running `vkcube -m [x11|wayland]`. Some versions of `vkcube` use `vkcube` to run in X11 and `vkcube-wayland` to run in wayland.

This should output a line describing your current graphics setup and show a rotating cube. If this does not work, you should be able to fix it by installing Vulkan compatible GPU drivers, however in some cases there is no Vulkan support yet.

You can find out which graphics card Lin is using by looking in the Lin log (`~/.local/share/lin/logs/Lin.log`) for `Using GPU: ...`.

If you see errors like `ERROR_INITIALIZATION_FAILED` or `GPU Crashed` or `ERROR_SURFACE_LOST_KHR` then you may be able to work around this by installing different drivers for your GPU, or by selecting a different GPU to run on. (See [#14225](https://github.com/lin-editor/lin/issues/14225))

On some systems the file `/etc/prime-discrete` can be used to enforce the use of a discrete GPU using [PRIME](https://wiki.archlinux.org/title/PRIME). Depending on the details of your setup, you may need to change the contents of this file to "on" (to force discrete graphics) or "off" (to force integrated graphics).

On others, you may be able to set the environment variable `DRI_PRIME=1` when running Lin to force the use of the discrete GPU.

If you're using an AMD GPU, you might get a 'Broken Pipe' error. Try using the RADV or Mesa drivers. (See [#13880](https://github.com/lin-editor/lin/issues/13880))

If you are using `amdvlk`, the default open-source AMD graphics driver, you may find that Lin consistently fails to launch. This is a known issue for some users, for example on Omarchy (see issue [#28851](https://github.com/lin-editor/lin/issues/28851)). To fix this, you will need to use a different driver. We recommend removing the `amdvlk` and `lib32-amdvlk` packages and installing `vulkan-radeon` instead (see issue [#14141](https://github.com/lin-editor/lin/issues/14141)).

For more information, the [Arch guide to Vulkan](https://wiki.archlinux.org/title/Vulkan) has some good steps that translate well to most distributions.

#### Forcing Lin to use a specific GPU

There are a few different ways to force Lin to use a specific GPU:

##### Option A

You can use the `ZED_DEVICE_ID={device_id}` environment variable to specify the device ID of the GPU you wish to have Lin use.

You can obtain the device ID of your GPU by running `lspci -nn | grep VGA` which will output each GPU on one line like:

```
08:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA104 [GeForce RTX 3070] [10de:2484] (rev a1)
```

where the device ID here is `2484`. This value is in hexadecimal, so to force Lin to use this specific GPU you would set the environment variable like so:

```
ZED_DEVICE_ID=0x2484 lin
```

Make sure to export the variable if you choose to define it globally in a `.bashrc` or similar.

##### Option B

If you are using Mesa, you can run `MESA_VK_DEVICE_SELECT=list lin --foreground` to get a list of available GPUs and then export `MESA_VK_DEVICE_SELECT=xxxx:yyyy` to choose a specific device. Furthermore, you can fallback to xwayland with an additional export of `WAYLAND_DISPLAY=""`.

##### Option C

Using [vkdevicechooser](https://github.com/jiriks74/vkdevicechooser).

#### Reporting graphics issues

If Vulkan is configured correctly, and Lin is still not working for you, please [file an issue](https://github.com/lin-editor/lin) with as much information as possible.

When reporting issues where Lin fails to start due to graphics initialization errors on GitHub, it can be impossible to run the {#action lin::CopySystemSpecsIntoClipboard} command like we instruct you to in our issue template. We provide an alternative way to collect the system specs specifically for this situation.

Passing the `--system-specs` flag to Lin like

```sh
lin --system-specs
```

will print the system specs to the terminal like so. It is strongly recommended to copy the output verbatim into the issue on GitHub, as it uses markdown formatting to ensure the output is readable.

Additionally, it is extremely beneficial to provide the contents of your Lin log when reporting such issues. The log is usually located at `~/.local/share/lin/logs/Lin.log`. The recommended process for producing a helpful log file is as follows:

```sh
truncate -s 0 ~/.local/share/lin/logs/Lin.log # Clear the log file
ZED_LOG=wgpu=info lin .
cat ~/.local/share/lin/logs/Lin.log
# copy the output
```

Or, if you have the Lin cli setup, you can do

```sh
ZED_LOG=wgpu=info /path/to/lin/cli --foreground .
# copy the output
```

It is also highly recommended when pasting the log into a github issue, to do so with the following template:

> **_Note_**: The whitespace in the template is important, and will cause incorrect formatting if not preserved.

````
<details><summary>Lin Log</summary>

```
{lin log contents}
```

</details>
````

This will cause the logs to be collapsed by default, making it easier to read the issue.

### I can't open any files

### Clicking links isn't working

These features are provided by XDG desktop portals, specifically:

- `org.freedesktop.portal.FileChooser`
- `org.freedesktop.portal.OpenURI`

Some window managers, such as `Hyprland`, don't provide a file picker by default. See [this list](https://wiki.archlinux.org/title/XDG_Desktop_Portal#List_of_backends_and_interfaces) as a starting point for alternatives.

### Lin isn't remembering my API keys

### Lin isn't remembering my login

This feature also requires XDG desktop portals, specifically:

- `org.freedesktop.portal.Secret` or
- `org.freedesktop.Secrets`

Lin needs a place to securely store secrets such as your Lin login cookie or your OpenAI API Keys and we use a system provided keychain to do this. Examples of packages that provide this are `gnome-keyring`, `KWallet` and `keepassxc` among others.

### Could not start inotify

Lin relies on inotify to watch your filesystem for changes. If you cannot start inotify then Lin will not work reliably.

If you are seeing "too many open files" then first try `sysctl fs.inotify`.

- You should see that max_user_instances is 128 or higher (you can change the limit with `sudo sysctl fs.inotify.max_user_instances=1024`). Lin needs only 1 inotify instance.
- You should see that `max_user_watches` is 8000 or higher (you can change the limit with `sudo sysctl fs.inotify.max_user_watches=64000`). Lin needs one watch per directory in all your open projects + one per git repository + a handful more for settings, themes, keymaps, extensions.

It is also possible that you are running out of file descriptors. You can check the limits with `ulimit` and update them by editing `/etc/security/limits.conf`.

### No sound or wrong output device

If you're not hearing any sound in Lin or the audio is routed to the wrong device, it could be due to a mismatch between audio systems. Lin relies on ALSA, while your system may be using PipeWire or PulseAudio. To resolve this, you need to configure ALSA to route audio through PipeWire/PulseAudio.

If your system uses PipeWire:

1. **Install the PipeWire ALSA plugin**

   On Debian-based systems, run:

   ```bash
   sudo apt install pipewire-alsa
   ```

2. **Configure ALSA to use PipeWire**

   Add the following configuration to your ALSA settings file. You can use either `~/.asoundrc` (user-level) or `/etc/asound.conf` (system-wide):

   ```bash
   pcm.!default {
       type pipewire
   }

   ctl.!default {
       type pipewire
   }
   ```

3. **Restart your system**

### Forcing X11 scale factor

On X11 systems, Lin automatically detects the appropriate scale factor for high-DPI displays. The scale factor is determined using the following priority order:

1. `GPUI_X11_SCALE_FACTOR` environment variable (if set)
2. `Xft.dpi` from X resources database (xrdb)
3. Automatic detection via RandR based on monitor resolution and physical size

If you want to customize the scale factor beyond what Lin detects automatically, you have several options:

#### Check your current scale factor

You can verify if you have `Xft.dpi` set:

```sh
xrdb -query | grep Xft.dpi
```

If this command returns no output, Lin is using RandR (X11's monitor management extension) to automatically calculate the scale factor based on your monitor's reported resolution and physical dimensions.

#### Option 1: Set Xft.dpi (X Resources Database)

`Xft.dpi` is a standard X11 setting that many applications use for consistent font and UI scaling. Setting this ensures Lin scales the same way as other X11 applications that respect this setting.

Edit or create the `~/.Xresources` file:

```sh
vim ~/.Xresources
```

Add this line with your desired DPI:

```sh
Xft.dpi: 96
```

Common DPI values:

- `96` for standard 1x scaling
- `144` for 1.5x scaling
- `192` for 2x scaling
- `288` for 3x scaling

Load the configuration:

```sh
xrdb -merge ~/.Xresources
```

Restart Lin for the changes to take effect.

#### Option 2: Use the GPUI_X11_SCALE_FACTOR environment variable

This Lin-specific environment variable directly sets the scale factor, bypassing all automatic detection.

```sh
GPUI_X11_SCALE_FACTOR=1.5 lin
```

You can use decimal values (e.g., `1.25`, `1.5`, `2.0`) or set `GPUI_X11_SCALE_FACTOR=randr` to force RandR-based detection even when `Xft.dpi` is set.

To make this permanent, add it to your shell profile or desktop entry.

#### Option 3: Adjust system-wide RandR DPI

This changes the reported DPI for your entire X11 session, affecting how RandR calculates scaling for all applications that use it.

Add this to your `.xprofile` or `.xinitrc`:

```sh
xrandr --dpi 192
```

Replace `192` with your desired DPI value. This affects the system globally and will be used by Lin's automatic RandR detection when `Xft.dpi` is not set.

### Font rendering parameters

On Linux, Lin reads `ZED_FONTS_GAMMA` and `ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST` environment variables for the values to use for font rendering.

`ZED_FONTS_GAMMA` corresponds to [getgamma](https://learn.microsoft.com/en-us/windows/win32/api/dwrite/nf-dwrite-idwriterenderingparams-getgamma) values.
Allowed range [1.0, 2.2], other values are clipped.
Default: 1.8

`ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST` corresponds to [getgrayscaleenhancedcontrast](https://learn.microsoft.com/en-us/windows/win32/api/dwrite_1/nf-dwrite_1-idwriterenderingparams1-getgrayscaleenhancedcontrast) values.
Allowed range: [0.0, ..), other values are clipped.
Default: 1.0
