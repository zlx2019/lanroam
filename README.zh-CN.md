# Lanroam

[![CI](https://github.com/zlx2019/lanroam/actions/workflows/ci.yml/badge.svg)](https://github.com/zlx2019/lanroam/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.96.0%2B-orange.svg)](https://www.rust-lang.org)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-3ccfbe)

[English](./README.md) · **简体中文**

> 一套键盘鼠标，控制局域网里的多台电脑。

把光标推出一块屏幕的边缘，它就落到下一台电脑上，键盘也随之切换过去。每台设备都是对等节点：不需要配置服务器，设备在局域网里自动互相发现，所有数据都经双向认证的 TLS 1.3 传输。Lanroam 是同系列的第三个局域网工具，前两个是 [Deskmate](https://github.com/zlx2019/deskmate)（文件传输）和 [Lanecho](https://github.com/zlx2019/lanecho)（剪贴板同步）。

## 状态

**早期开发中，暂不可用。** 设备发现、设备身份、绑定证书指纹的 QUIC 传输已经完成。在命令行里，设备可以组成桌面组（用 PIN 加入）、共享屏幕布局并互相控制：光标能在 Mac 与 Windows 之间、朝任意方向穿过相接的屏幕边缘，也能用快捷键直接跳转。下一步是桌面端应用。

| 里程碑 | 内容 | |
|---|---|---|
| M0 | 共享的局域网基础库、QUIC 传输、集成测试用 CLI | 已完成 |
| M1 | 键鼠捕获与注入原型（macOS ↔ Windows） | 已完成 |
| M2 | 桌面组、屏幕布局、边缘穿越、快捷键 | 已完成 |
| M3 | 桌面端：屏幕排列、配对、托盘 | 下一步 |
| M4 | 剪贴板交接 | |
| M5 | 设备之间直接拖拽文件 | |

## 工作区

```text
deps/lan-kit        共享的局域网基础库：身份、双向 TLS 1.3、发现、分帧
deps/lanroam-input  键盘与鼠标：捕获、注入、键位映射、边缘切换
deps/lanroam-core   引擎：桌面组、QUIC 传输、协议、输入会话、诊断
deps/lanroam-cli    命令行工具，用于协议调试和集成测试
```

## 开发

Rust 版本由 `rust-toolchain.toml` 固定，所需工具见 [CONTRIBUTING.md](./CONTRIBUTING.md)。

```bash
cargo nextest run --workspace          # 运行测试
cargo run -p lanroam-cli -- run        # 让本机以桌面组成员身份运行（控制台命令：join、layout、place……）
cargo run -p lanroam-cli -- scan       # 列出局域网里的节点
cargo run -p lanroam-cli -- ping <名称 | 指纹前缀 | ip:端口>
```

在 `run` 里，`join <设备>` 请求附近的一台设备让本机加入它的组（两台都没有组时会新建一个）；那台设备会显示一个 6 位 PIN，在这边输入即可。PIN 通过一次绑定双方 TLS 证书的口令认证密钥交换（PAKE）来校验，所以冒充你所选设备的第三方拿不到任何可利用的信息。`layout` 显示每个成员的屏幕在共享画布上的位置（按逻辑像素计算，150% 缩放的 Windows 屏幕也能和 Mac 对齐）以及它们相接的边；`place <成员> right-of <成员> [偏移]`（或 `left-of`、`above`、`below`）调整摆放，对整个组生效。

`run` 运行期间，把光标推出本机与其他成员相接的边，就开始控制那台设备；可以继续穿到更远的设备再穿回来，位置沿相接的边按比例对应。在被控设备上动一下它自己的鼠标或键盘，控制权就回到它本机。快捷键（Mac 上 Alt 即 Option）：

| 按键 | 作用 |
|---|---|
| Ctrl+Alt+1..9 | 跳到第 n 台设备，编号与 `layout` 显示的一致 |
| Ctrl+Alt+方向键 | 跳到该方向上的相邻设备 |
| Ctrl+Alt+L、Scroll Lock | 把光标锁定在当前设备上，或解锁 |
| Ctrl+Alt+Esc | 回到本机并暂停边缘穿越，或恢复 |

数字、方向键和 L 需要按左 Alt：很多键盘布局里 AltGr（Windows 上等于 Ctrl+右 Alt）配合这些键是用来输入字符的。Mac 与 PC 之间会互换 Command 和 Control，让快捷键还在原来的手指位置（`swap off` 可以关闭输入到本机时的互换）。在 macOS 上，运行命令的应用（也就是你的终端）需要在“系统设置 > 隐私与安全性”里获得“辅助功能”和“输入监控”权限。

在同一台机器上跑两个实例时，它们需要各自的身份：给每个实例传不同的 `--data-dir`，并使用 `--port 0`；`run --dry-run` 不捕获本机输入，只把收到的输入打印出来，而不是注入。

Windows 部分的代码在任何机器上都能做静态检查：`cargo clippy -p lanroam-input --target x86_64-pc-windows-msvc`。

### Windows 开发版

每次推送都会构建 `lanroam-cli.exe`，并发布到滚动更新的 [`dev` 预发布版](https://github.com/zlx2019/lanroam/releases/tag/dev)。在 Windows 上用下面的命令获取最新版：

```powershell
irm "https://raw.githubusercontent.com/zlx2019/lanroam/main/scripts/windows/update.ps1?$(Get-Random)" | iex
```

第一次运行时，请在管理员权限的 PowerShell 里加上 `-Firewall` 参数，放行入站流量（见脚本开头的说明）。

## 参与贡献

工作区结构、CI 会跑的检查和开发约定见 [CONTRIBUTING.md](./CONTRIBUTING.md)。安全问题请按 [SECURITY.md](./SECURITY.md) 的说明私下报告。

## 许可证

[MIT](./LICENSE)
