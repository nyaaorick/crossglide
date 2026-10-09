//! Starting the tray app at login: a `Run` registry value on Windows, a LaunchAgent on macOS.
//! Windows starts this same executable, wherever it was built. macOS starts a copy of it in the
//! config directory (see `platform::installed`).

use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

fn exe() -> Result<PathBuf> {
    std::env::current_exe().context("can't find this program's path")
}

#[cfg(windows)]
mod platform {
    use anyhow::{Context, Result};
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Crossglide";

    pub fn enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(KEY, KEY_READ)
            .and_then(|key| key.get_value::<String, _>(VALUE))
            .is_ok()
    }

    /// Windows starts the build itself, wherever it is, so there's nothing to update.
    pub fn refresh() -> Result<()> {
        Ok(())
    }

    pub fn set(on: bool) -> Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(KEY, KEY_SET_VALUE)
            .context("can't open the Run key")?;
        if on {
            let command = format!("\"{}\"", super::exe()?.display());
            key.set_value(VALUE, &command)
                .context("can't write the Run value")
        } else {
            match key.delete_value(VALUE) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).context("can't delete the Run value")
                }
                _ => Ok(()),
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};

    const LABEL: &str = "com.crossglide.tray";

    /// The copy of the tray app that login starts. A checkout usually lives in Desktop or
    /// Documents, which macOS guards: a program started there at login, with no Terminal around
    /// it, hangs until someone answers the folder-access prompt. The config directory isn't
    /// guarded. The copy keeps the build's signature, so its Accessibility permission carries over
    /// as long as the build is signed with a certificate (scripts/sign-mac.sh).
    fn installed() -> Result<PathBuf> {
        Ok(crossglide_agent::config::default_dir()?.join("crossglide"))
    }

    /// Copies this build over the installed one, if it's a different file. It's written beside it
    /// and renamed over it: overwriting a program in place, even one that isn't running, can get
    /// it killed for an invalid signature when it next starts.
    fn install(exe: &Path, to: &Path) -> Result<()> {
        if fs::read(exe).ok() == fs::read(to).ok() {
            return Ok(());
        }
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        let partial = to.with_extension("new");
        fs::copy(exe, &partial).with_context(|| format!("can't copy to {}", partial.display()))?;
        fs::rename(&partial, to).with_context(|| format!("can't replace {}", to.display()))
    }

    /// With Start at login on, makes login start this build from now on.
    pub fn refresh() -> Result<()> {
        if !enabled() {
            return Ok(());
        }
        let exe = super::exe()?;
        let to = installed()?;
        if exe != to {
            install(&exe, &to)?;
        }
        set_plist(&to)
    }

    fn plist() -> Result<PathBuf> {
        Ok(dirs::home_dir()
            .context("no home directory")?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")))
    }

    pub fn enabled() -> bool {
        plist().is_ok_and(|p| p.exists())
    }

    pub fn set(on: bool) -> Result<()> {
        let path = plist()?;
        if !on {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("can't delete {}", path.display()))
                }
                _ => Ok(()),
            };
        }
        let to = installed()?;
        install(&super::exe()?, &to)?;
        set_plist(&to)
    }

    fn set_plist(exe: &Path) -> Result<()> {
        let path = plist()?;
        let exe = exe
            .to_str()
            .context("this program's path isn't valid UTF-8")?
            .replace('&', "&amp;")
            .replace('<', "&lt;");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
        );
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, plist).with_context(|| format!("can't write {}", path.display()))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use anyhow::{Result, bail};

    pub fn enabled() -> bool {
        false
    }

    pub fn refresh() -> Result<()> {
        Ok(())
    }

    pub fn set(_on: bool) -> Result<()> {
        bail!("starting at login isn't supported on this system")
    }
}

pub use platform::{enabled, refresh, set};
