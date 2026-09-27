# Lanroam

[![CI](https://github.com/zlx2019/lanroam/actions/workflows/ci.yml/badge.svg)](https://github.com/zlx2019/lanroam/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.96.0%2B-orange.svg)](https://www.rust-lang.org)

> Share one keyboard and mouse across the computers on your LAN.

Move the pointer past the edge of one screen and it lands on the next computer, with the keyboard following along. Every device is an equal peer: there is no server to configure, devices find each other on the LAN, and everything travels over mutually authenticated TLS 1.3. Lanroam is the third LAN tool in the family, after [Deskmate](https://github.com/zlx2019/deskmate) (file transfer) and [Lanecho](https://github.com/zlx2019/lanecho) (clipboard sync).

## Status

**Early development, not usable yet.** Peer discovery, device identity and a QUIC transport pinned to certificate fingerprints are done. The keyboard and mouse prototype works from the command line between a Mac and a Windows PC, in both directions. Desk groups (joining with a PIN, kicking, leaving) and a shared screen layout work from the command line; switching between any devices of a group and the desktop app come next.

| Milestone | Scope | |
|---|---|---|
| M0 | Shared LAN foundation, QUIC transport, integration CLI | done |
| M1 | Input capture and injection prototype (macOS ↔ Windows) | done |
| M2 | Desk groups, screen layout, edge crossing, hotkeys | in progress |
| M3 | Desktop app: screen arrangement, pairing, tray | |
| M4 | Clipboard hand-off | |
| M5 | Drag and drop files between devices | |

## Workspace

```text
deps/lan-kit        shared LAN foundation: identity, mutual TLS 1.3, discovery, framing
deps/lanroam-input  keyboard and mouse: capture, injection, key maps, edge switching
deps/lanroam-core   the engine: desk groups, QUIC transport, protocol, input sessions, diagnostics
deps/lanroam-cli    command-line tool for protocol debugging and integration tests
```

## Develop

Rust is pinned by `rust-toolchain.toml`; see [CONTRIBUTING.md](./CONTRIBUTING.md) for the tooling.

```bash
cargo nextest run --workspace          # tests
cargo run -p lanroam-cli -- run        # run this device in its desk group (console: join, layout, place, ...)
cargo run -p lanroam-cli -- listen     # run a node for the input prototype
cargo run -p lanroam-cli -- scan       # list nodes on the LAN
cargo run -p lanroam-cli -- ping <name | fingerprint prefix | ip:port>
cargo run -p lanroam-cli -- share <target> --edge right   # control <target> from here
```

In `run`, `join <device>` asks a nearby device to let this one into its group (founding a group when neither has one); that device shows a 6-digit PIN to type here. The PIN is checked with a password-authenticated key exchange bound to both TLS certificates, so a device impersonating the one you picked learns nothing it could use. `layout` shows where every member's screens sit on a shared canvas (in logical pixels, so a 150% Windows display lines up with a Mac's) and which edges they share; `place <member> right-of <member> [offset]` (or `left-of`, `above`, `below`) rearranges them, for the whole group.

Two instances on one machine need their own identities: pass a different `--data-dir` to each and `--port 0`; `listen --dry-run` prints the input it receives instead of injecting it.

`share` captures the local keyboard and mouse: push the pointer through the chosen edge to control the target, move it back to return. **Ctrl+Alt+Esc** (Ctrl+Option+Esc on a Mac) takes control back at once. On macOS, the app running the command (your terminal) needs Accessibility (both to control and to be controlled) and Input Monitoring (to control) under System Settings > Privacy & Security.

The Windows code can be linted from any machine: `cargo clippy -p lanroam-input --target x86_64-pc-windows-msvc`.

### Windows dev builds

Every push builds `lanroam-cli.exe` and publishes it to the rolling [`dev` pre-release](https://github.com/zlx2019/lanroam/releases/tag/dev). On a Windows machine, fetch the latest one with:

```powershell
irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)" | iex
```

The first time, run it from an elevated PowerShell with `-Firewall` to allow inbound traffic (see the script header).

## License

[MIT](./LICENSE)
