<div align="center">

# Crossglide

**让 MacBook 的触控板、键盘和扬声器,直接为你的 Windows 电脑所用。**

把指针从 Mac 屏幕边缘滑出去,它就出现在 PC 上,由真正的 MacBook 触控板驱动。
PC 的声音从 Mac 的扬声器里播放出来。一切都走同一条局域网内的加密连接,全部用 Rust 编写。

[![CI](https://github.com/nyaaorick/crossglide/actions/workflows/ci.yml/badge.svg)](https://github.com/nyaaorick/crossglide/actions/workflows/ci.yml)
[![License: GPL v2](https://img.shields.io/badge/license-GPL--2.0-blue.svg)](LICENSE)
![Status](https://img.shields.io/badge/status-alpha-yellow.svg)

[English](README.md) · **简体中文**

</div>

---

## 为什么选择 Crossglide

普通的软件 KVM 只是移动鼠标指针,Crossglide 移动的是**触控板**。

- **Windows 上的真·精确式触摸板。** Mac 把手指的原始触点发给 PC 上的一个小型虚拟触摸板驱动,Windows 把它当作真正的 Precision Touchpad,手势由系统自己识别:双指滚动、捏合缩放、三指和四指滑动、轻点点按。不需要自己写、也不会写错手势逻辑。
- **PC 的声音从 Mac 播放。** PC 的系统音频被捕获、用 Opus 编码,再由 MacBook 的扬声器播放,带自适应抖动缓冲和时钟漂移补偿。在 Wi-Fi 上实测端到端 **41–76 ms**。
- **滑一下就切换。** 把指针推过屏幕边缘,控制权就交给 PC,那条边上会出现一条毛玻璃提示。快捷键(`ctrl+option+cmd+space`)可随时双向切换。
- **一条私密连接。** 全部走 QUIC,并固定证书指纹:触控走低延迟数据报,按键走可靠流,音频走数据报。没有云,也不需要账号。
- **两端都有托盘应用。** 一眼看到状态,可开关音频、开机自启,在 Windows 上还有一键 **安装触摸板驱动**。
- **从上到下都是 Rust**(唯独约 500 行的 Windows 驱动暂时必须用 C)。

## 状态

Crossglide 目前是 **alpha**,暂时只支持从源码运行,还没有安装包或发布版。

| 功能 | 状态 |
| --- | --- |
| 加密的旁路通道(QUIC、指纹固定、自动重连、时钟同步) | 可用 |
| PC 音频 → Mac 扬声器 | 可用。长时间漂移和有线网络的数据还待测 |
| 托盘应用:状态、音频开关、开机自启 | 可用 |
| 触控板 → 虚拟 Windows 精确式触摸板 | 可用:指针、边缘和快捷键切换、键盘。完整手势仍在测试 |
| 边缘毛玻璃提示 | 已实现,测试中 |
| 从托盘安装触摸板驱动(Windows) | 可用,已在 PC 上验证 |
| 配对码(目前指纹需手动复制) | 计划中 |
| 剪贴板及 Deskflow 核心的其余功能 | 计划中 |

已在一台 MacBook Air(macOS 26)和一台 Windows 11 电脑(build 26200)上测试,两台机器在同一个 Wi-Fi 下。测量数据和后续计划见 [ROADMAP.md](ROADMAP.md)。

## 快速开始

需要同一网络里的两台机器:一台 **MacBook** 和一台 **Windows 11 电脑**。

**两台机器都要做**

1. 安装 [Rust](https://rustup.rs)、[`just`](https://github.com/casey/just)、CMake 和 git。Windows 上还要装 Visual Studio C++ 生成工具(Rust 安装器会提示)。
2. `git clone https://github.com/nyaaorick/crossglide.git && cd crossglide`
3. 运行 `just dev fingerprint`,记下本机的指纹。
4. 运行一次 `just dev`,它会生成带注释的 `agent.toml` 后退出。在两边分别把 `peer_fingerprint` 填成**对方**的指纹;PC 上还要把 `server` 填成 Mac 的 IP 地址。

**然后启动托盘应用**

| Mac | Windows 电脑 |
| --- | --- |
| `just tray`,或双击 `scripts/tray.command` | 把 `scripts\tray.ps1` 拖进 PowerShell |
| 提示时给终端开启**辅助功能**权限(系统设置 → 隐私与安全性),然后重新启动 | 在托盘菜单里选 **Install touchpad driver**(只需一次) |

两边的托盘图标变绿就表示连上了,PC 的声音会开始在 Mac 上播放。把指针推过 Mac 屏幕的左边或右边,或按 `ctrl+option+cmd+space`,MacBook 的触控板和键盘就在控制 PC 了。

设置在 `agent.toml` 里:Mac 上是 `~/Library/Application Support/crossglide`,PC 上是 `%APPDATA%\crossglide`(`[touch]` 里的 `edges`、`hotkey`,以及 Command 键在 PC 上当作 Windows 键还是 Ctrl)。日志在同一目录的 `agent.log`。

## 了解更多

- 架构、取舍和每个决定的理由:[docs/DESIGN.md](docs/DESIGN.md)(英文)
- 里程碑、实测数据和下一步:[ROADMAP.md](ROADMAP.md)(英文)
- 欢迎提交 issue 和 PR,尤其欢迎其他型号 Mac 和 PC 上的测试报告。提交前请运行 `just lint`、`just test` 和 `just deny`。

## 许可证

[GPL-2.0-only](LICENSE),与 Deskflow 相同。
