> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

# Lin

Lin is a local-first code editor and agent harness built on the Lin editor core.

It keeps the editor, the language tooling, the ACP agent harness, and third-party model
providers. Lin's hosted services have been removed: there is no account system, no
subscription or membership UI, no hosted AI or edit predictions, no telemetry or crash
reporting, and no automatic updates. The extension system is unchanged and still uses the
official Lin extension registry.

The application, its data directories, and its command line tool are all named `lin`, so it
can run side by side with an installed Lin.

## Building

- macOS: `cargo run` from the repository root, or `script/bundle-mac` to produce a `.dmg`.
- Linux: `script/bundle-linux` produces a tarball.

Development notes live under [`docs/src/development`](./docs/src/development).

## Licensing

Lin is distributed under the GNU General Public License v3.0 or later. It is derived from
the Lin editor, Copyright Lin Industries, Inc., and from other contributors. Third-party
license texts are collected in [`assets/licenses.md`](./assets/licenses.md).
