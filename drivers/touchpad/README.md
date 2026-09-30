# Virtual precision touchpad (Windows)

A small UMDF 2 HID driver that makes Windows see a **Windows Precision Touchpad**, so the Mac's trackpad gets Windows' own gestures: two-finger scroll, pinch zoom, three- and four-finger swipes, and tap to click. It has no touch logic of its own. The crossglide agent writes finished touchpad reports to a vendor collection on the same device (`crates/touch/src/win.rs`), and the driver passes each one to Windows as input.

| File | What it is |
| --- | --- |
| `touchpad.c` | The driver: HID descriptors, feature reports and the report pipe |
| `crossglide-touchpad.inf` | Installs it on the root-enumerated device `Root\CrossglideTouchpad` |
| `build.ps1` | Builds, signs with a local test certificate, and installs or updates it |

## Install

In an **administrator** PowerShell on the PC, from the repo:

```powershell
powershell -ExecutionPolicy Bypass -File drivers\touchpad\build.ps1
```

The first run downloads the WDK's NuGet package (110 MB) into `~\wdk`. It also creates the certificate `CN=Crossglide Test Signing` and trusts it on this machine only. Run the same command after changing the driver to rebuild and update it. `-NoInstall` only builds; `-Uninstall` removes the device and the driver package.

It needs the Visual Studio C++ Build Tools, which the Rust toolchain already uses. Test-signing mode **isn't** needed: a user-mode driver loads with a locally trusted certificate (checked on Windows 11 26200, 2026-09-28).

Check it worked: Device Manager shows *Crossglide Virtual Touchpad* under Human Interface Devices. The System event log also has *"Touch/Touchpad Hardware Quality Assurance verification succeeded"* from Win32k. `cargo run -p crossglide-touch --example inject` must be run in the desktop session, not over SSH. It slides a finger and prints where the pointer went.

## Install from the tray app

`build.ps1` is the developer's route: it needs the WDK and an administrator PowerShell. For a PC that only runs crossglide, the Windows tray app gets an **Install touchpad driver** item, written in Rust. The built and signed driver is committed in `drivers/touchpad/package/` (DLL, `.inf`, `.cat`, `.cer`; copy the first three from `target\touchpad\package` after a change to the driver, and export the public certificate with `Export-Certificate -Cert (Get-ChildItem Cert:\LocalMachine\My | ? Subject -eq 'CN=Crossglide Test Signing') -FilePath crossglide-touchpad.cer`) and embedded in the tray app, so a `git pull` and the menu item are all a PC needs, with a UAC prompt for the install. The design, and what it trusts, is in the [design doc](../../docs/DESIGN.md#installing-the-driver-from-the-tray); the task list is in the [ROADMAP](../../ROADMAP.md#touchpad-m10-brought-forward).

## Reports

| ID | Collection | Direction | Contents |
| --- | --- | --- | --- |
| 1 | Touch pad | Input | 5 contacts × (confidence, tip, contact ID, X, Y), scan time, contact count, button |
| 2 | Mouse | Input | 3 buttons, X, Y, wheel; required for legacy mode, unused so far |
| 3 | Touch pad | Feature | Contact count maximum (5), pad type (click pad) |
| 4 | Touch pad | Feature | The certification blob every open-source driver returns |
| 5 | Configuration | Feature | Input mode, set by Windows |
| 6 | Configuration | Feature | Surface and button switches, set by Windows |
| 7 | Feed (0xFF42) | Output | From the agent: an input report (ID first), padded to 32 bytes |

X and Y are in 0.05 mm on the 121.9 × 74.1 mm surface of a MacBook Air 13" (M4); other Macs' trackpads are scaled to it. `crates/touch/src/report.rs` builds the reports and must match the descriptor.
