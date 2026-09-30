//! Installing the virtual touchpad's driver from the tray (Windows only; see drivers/touchpad).
//! The signed driver package is committed in `drivers/touchpad/package` and built into this
//! program, so there's nothing to download. The tray asks, then starts a copy of itself with
//! `--install-driver` and Windows' permission prompt; that copy does the installing as an
//! administrator and the tray hears how it went from its exit code.

use anyhow::Result;

/// Starts the elevated copy that does the installing.
pub const FLAG: &str = "--install-driver";

/// Whether this system can install the driver.
pub const fn supported() -> bool {
    cfg!(windows)
}

/// Asks, installs in an elevated copy of this program, and calls `done` with how it went.
/// Returns at once; the work happens on a thread of its own.
pub fn start(done: impl FnOnce(Result<()>) + Send + 'static) {
    std::thread::spawn(move || done(platform::install_elevated()));
}

/// The elevated copy: installs the driver and returns the exit code.
pub fn run_elevated() -> i32 {
    platform::run_elevated()
}

#[cfg(windows)]
mod platform {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::{env, fs};

    use anyhow::{Context, Result, bail};
    use crossglide_agent::app::{self, LOG_FILE};
    use crossglide_agent::config;
    use tracing::{error, info};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDOK, MB_ICONQUESTION, MB_OKCANCEL, MessageBoxW, SW_SHOWNORMAL,
    };

    /// The driver package, as committed in the repo.
    const FILES: [(&str, &[u8]); 4] = [
        (
            "crossglide-touchpad.dll",
            include_bytes!("../../../drivers/touchpad/package/crossglide-touchpad.dll"),
        ),
        (
            "crossglide-touchpad.inf",
            include_bytes!("../../../drivers/touchpad/package/crossglide-touchpad.inf"),
        ),
        (
            "crossglide-touchpad.cat",
            include_bytes!("../../../drivers/touchpad/package/crossglide-touchpad.cat"),
        ),
        (
            "crossglide-touchpad.cer",
            include_bytes!("../../../drivers/touchpad/package/crossglide-touchpad.cer"),
        ),
    ];

    /// `pnputil`'s "done, but restart to finish" exit code.
    const RESTART_NEEDED: i32 = 3010;
    /// `pnputil /install` has nothing to do: every device already runs this driver.
    const UP_TO_DATE: i32 = 259;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// Asks first: installing the driver also makes this PC trust its certificate.
    fn confirmed() -> bool {
        let text = wide(
            "Install the Crossglide touchpad driver?\n\nThis makes this PC trust the certificate \
             \"Crossglide Test Signing\" that comes with the crossglide repo, and installs the \
             driver signed with it. Windows will ask for permission next.",
        );
        let title = wide("Crossglide");
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OKCANCEL | MB_ICONQUESTION,
            )
        };
        answer == IDOK
    }

    pub fn install_elevated() -> Result<()> {
        if !confirmed() {
            bail!("cancelled");
        }
        let exe = wide(
            env::current_exe()
                .context("can't find this program's path")?
                .to_str()
                .context("this program's path isn't valid UTF-8")?,
        );
        let verb = wide("runas");
        let params = wide(super::FLAG);
        // SAFETY: an all-zero SHELLEXECUTEINFOW is valid once its size and the fields used are
        // set; the strings outlive the call, and the process handle is closed below.
        let code = unsafe {
            let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
            info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_NOCLOSEPROCESS;
            info.lpVerb = verb.as_ptr();
            info.lpFile = exe.as_ptr();
            info.lpParameters = params.as_ptr();
            info.nShow = SW_SHOWNORMAL;
            if ShellExecuteExW(&mut info) == 0 || info.hProcess.is_null() {
                bail!("Windows didn't start the installer (was the permission prompt refused?)");
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1;
            GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hProcess);
            code
        };
        if code != 0 {
            bail!("the installer failed (exit code {code}); the log says why");
        }
        Ok(())
    }

    pub fn run_elevated() -> i32 {
        // The same log as the tray's, so the reason for a failure is where the user looks.
        if let Ok(dir) = config::default_dir() {
            let _ = app::start_logging(Some(&dir.join(LOG_FILE)));
        }
        match install() {
            Ok(()) => {
                info!("touchpad driver installed");
                0
            }
            Err(e) => {
                error!("touchpad driver: {e:#}");
                1
            }
        }
    }

    fn install() -> Result<()> {
        let dir = package_dir()?;
        fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
        for (name, bytes) in FILES {
            fs::write(dir.join(name), bytes).with_context(|| format!("can't write {name}"))?;
        }
        let system =
            PathBuf::from(env::var_os("SystemRoot").context("no SystemRoot")?).join("System32");
        let cer = dir.join("crossglide-touchpad.cer");
        for store in ["Root", "TrustedPublisher"] {
            run(
                &system.join("certutil.exe"),
                &["-addstore", "-f", store, &cer.to_string_lossy()],
            )?;
        }
        if crossglide_touch::install::ensure_device().context("can't create the device")? {
            info!("touchpad driver: created the device");
        }
        let inf = dir.join("crossglide-touchpad.inf");
        run(
            &system.join("pnputil.exe"),
            &["/add-driver", &inf.to_string_lossy(), "/install"],
        )
    }

    /// Where the package is unpacked: under Program Files, which only administrators can write
    /// to, so nobody can swap a file between here and the installing.
    fn package_dir() -> Result<PathBuf> {
        Ok(
            PathBuf::from(env::var_os("ProgramFiles").context("no ProgramFiles")?)
                .join("Crossglide")
                .join("driver"),
        )
    }

    fn run(program: &Path, args: &[&str]) -> Result<()> {
        let name = program.file_name().unwrap_or_default().to_string_lossy();
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("can't run {name}"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            info!("{name}: {line}");
        }
        match output.status.code() {
            Some(0 | UP_TO_DATE) => Ok(()),
            Some(RESTART_NEEDED) => {
                info!("{name}: Windows needs a restart to finish");
                Ok(())
            }
            code => bail!("{name} failed (exit code {code:?}): {}", text.trim()),
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use anyhow::{Result, bail};

    pub fn install_elevated() -> Result<()> {
        bail!("the touchpad driver is for Windows PCs")
    }

    pub fn run_elevated() -> i32 {
        eprintln!("error: the touchpad driver is for Windows PCs");
        1
    }
}
