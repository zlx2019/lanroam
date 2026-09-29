<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/logo-wordmark-dark.svg">
    <img src="./assets/logo-wordmark.svg" alt="Lanroam" width="460">
  </picture>
</h1>
<p align="center">
  Keyboard and mouse sharing over your LAN — roam freely across the devices within reach.
</p>


<p align="center">
  <a href="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/lanroam/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/lanroam/releases"><img src="https://img.shields.io/github/v/release/zlx2019/lanroam?color=12B5A2" alt="Release" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows-12B5A2" alt="Platform" />
  <img src="https://img.shields.io/badge/Rust-1.96-dea584?logo=rust&logoColor=white" alt="Rust 1.96" />
  <img src="https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white" alt="Tauri 2" />
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
</p>

<p align="center">
  <b>English</b> · <a href="./README.zh-CN.md">简体中文</a>
</p>

---

**Lanroam** is a free, open-source keyboard and mouse sharing app: control every device around you with one computer's mouse, keyboard or touchpad, and take the clipboard and files along.

Every device running Lanroam is an equal peer on your local network. Your devices join one desk group, and their screens are laid out on a shared canvas the way they sit on your desk. Devices talk to each other directly, over mutually authenticated TLS 1.3 by default.

## ✨ Features

- **Easy control** — move the pointer off one screen and onto the next to switch devices, or jump there with a hotkey.
- **Clipboard sharing** — text, images and screenshots, and files up to 32 MB, copied on any device come along with the pointer to the others.
- **File transfer** — drag files or folders straight past the edge of the screen and drop them on the other computer: its desktop, a folder or an app window.
- **LAN P2P** — every device is an equal peer; no server, no cloud, no account. Devices coming and going show up live, and dropped connections come back on their own.
- **Zero-config discovery** — mDNS first, UDP multicast as a fallback; nearby devices just show up.
- **Join with a PIN** — on a new device, type the 6-digit PIN a group member shows to join; the PIN is checked with a password-authenticated key exchange bound to both TLS certificates.
- **Secure by default** — mutually authenticated TLS 1.3 over QUIC, with device identities pinned to certificate fingerprints; input only ever reaches the members of your group.
- **Lives in the menu bar** — pause, lock or switch to a device from the menu bar (macOS) or the tray (Windows), and optionally start at login.

### ⌨️ Default hotkeys

Change them to your own under **Settings → Control**; on a Mac, Alt is the Option key.

| Keys | Action |
|---|---|
| `Ctrl+Alt+1..9` | Control device N (numbered on the **Arrange** page) |
| `Ctrl+Alt+Arrow` | Control the neighbouring device in that direction |
| `Ctrl+Alt+L`, `Scroll Lock` | Lock the pointer to the current device; again to unlock |
| `Ctrl+Alt+Esc` | Back to this device and pause switching; again to resume |

## 📥 Install

Download from [Releases](https://github.com/zlx2019/lanroam/releases).

| Platform | Requires | Artifact |
|---|---|---|
| macOS (Apple Silicon) | macOS 13 Ventura or later | `Lanroam-x.y.z-macos-aarch64.dmg` |
| macOS (Intel) | macOS 13 Ventura or later | `Lanroam-x.y.z-macos-x64.dmg` |
| Windows | Windows 10 or later, x64 | `Lanroam-x.y.z-windows-x64-setup.exe` |

Each release carries the command-line node (`lanroam-cli`) too; see [CONTRIBUTING.md](./CONTRIBUTING.md#the-command-line-node).

### 🍎 macOS

Drag Lanroam into Applications. On first launch, it walks you through the **Accessibility** and **Input Monitoring** permissions it needs to capture and inject input.

### 🪟 Windows

The installer lets Lanroam through the firewall on private networks, which discovery and control need.

> Builds are not signed yet. On macOS, open the app with right-click → **Open** the first time (on macOS 15 and later: System Settings → Privacy & Security → **Open Anyway**), or run `xattr -cr /Applications/Lanroam.app`. Windows SmartScreen may ask for confirmation as well.

## 🚀 Quick start

1. Install and open Lanroam on every computer on the same LAN.
2. On the **Devices** page, pick a device under **Nearby**, click **Join**, and type the 6-digit PIN shown on its screen.
3. On the **Arrange** page, drag the screens to where they sit on your desk.
4. Push the pointer past the edge where two screens meet, and it lands on the other computer.

## ❓ FAQ

**The pointer stops at the edge of the Mac, or keys never reach the other device.**
Lanroam needs **Accessibility** and **Input Monitoring** under System Settings → Privacy & Security, and macOS applies them only after the app restarts (Lanroam offers to). When you use the CLI, it is your terminal that needs them.

**The permissions stopped working after an update (macOS).**
Builds are only ad-hoc signed for now, which ties the grants to the exact binary, so every new build invalidates them, and re-ticking the stale entry does nothing. Remove Lanroam from both lists, launch it again, grant the fresh entries, then restart Lanroam.

**Devices never show up on macOS.**
macOS 15+ asks for **Local Network** permission on first launch. It must be allowed, otherwise discovery fails silently. Re-enable it under System Settings → Privacy & Security → Local Network.

**Devices never show up on Windows.**
Discovery and control need an inbound firewall rule for private networks, which the installer adds. Make sure Windows counts your network as **Private**, not Public.

## 🔨 Build from source

The desktop app is Tauri 2 + React (`apps/desktop`), on a UI-free engine shared with the command-line node:

```text
deps/lan-kit        shared LAN foundation: identity, mutual TLS 1.3, discovery, framing
deps/lanroam-input  keyboard and mouse: capture, injection, key maps, edge switching
deps/lanroam-core   the engine: desk groups, QUIC transport, protocol, input sessions
deps/lanroam-cli    command-line node for protocol debugging and tests between machines
apps/desktop        the desktop app
```

Rust is pinned by `rust-toolchain.toml` (1.96, edition 2024); the app also needs Node 22+ and pnpm.

```bash
cd apps/desktop && pnpm install && pnpm tauri build  # the app for the current platform
cargo build --release -p lanroam-cli                 # the command-line node
```

## 🤝 Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for running the app and the CLI during development, the checks CI runs and the conventions. Please report security issues privately as described in [SECURITY.md](./SECURITY.md).

## 📄 License

[MIT](./LICENSE)
