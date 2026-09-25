//! Starting the tray app at login: a `Run` registry value on Windows, a LaunchAgent on macOS.
//! Both start this same executable, wherever it was built.

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
    use std::path::PathBuf;

    use anyhow::{Context, Result};

    const LABEL: &str = "com.crossglide.tray";

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
        let exe = super::exe()?;
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

    pub fn set(_on: bool) -> Result<()> {
        bail!("starting at login isn't supported on this system")
    }
}

pub use platform::{enabled, set};
