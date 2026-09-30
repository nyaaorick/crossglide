//! `crossglide`: the tray app. It runs the agent and shows how it's doing in the Windows
//! notification area (or the Mac's menu bar), with a few controls. The log goes to `agent.log`
//! in the config folder, as with `crossglide-agent`.

// No console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod driver;
mod hint;

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossglide_agent::app::{self, LOG_FILE};
use crossglide_agent::audio::AudioStatus;
use crossglide_agent::config;
use crossglide_agent::identity::Identity;
use crossglide_agent::link::{self, Mode, Settings, Status};
use crossglide_agent::touch::{self, Hint};
use crossglide_audio::device::Source;
use hint::HintWindow;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// How often the tray reads the agent's state.
const REFRESH: Duration = Duration::from_millis(500);
/// Held while the tray app runs, in the config folder, so only one runs per user.
const LOCK_FILE: &str = "crossglide.lock";
/// Longest status line shown in the menu.
const LINE: usize = 90;

fn main() {
    // The elevated copy the tray starts to install the touchpad driver: no tray, no lock.
    if std::env::args().any(|arg| arg == driver::FLAG) {
        std::process::exit(driver::run_elevated());
    }
    let dir = match config::default_dir() {
        Ok(dir) => dir,
        Err(e) => return eprintln!("error: {e:#}"),
    };
    // A second copy would fight the first one for the connection.
    let _lock = match lock(&dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => return eprintln!("crossglide is already running"),
        Err(e) => return eprintln!("error: can't lock {}: {e:#}", dir.display()),
    };
    if let Err(e) = app::start_logging(Some(&dir.join(LOG_FILE))) {
        eprintln!("error: {e:#}");
    }
    // Without a console, a panic would vanish; put it in the log.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        error!("crossglide crashed: {panic}");
        default_hook(panic);
    }));
    info!("tray app started");
    run(Agent::new(&dir), dir);
}

fn lock(dir: &Path) -> Result<Option<File>> {
    fs::create_dir_all(dir)?;
    let file = File::create(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The agent, running on a tokio runtime beside the tray's event loop.
struct Agent {
    runtime: Runtime,
    /// The settings from `agent.toml`, or why they couldn't be read.
    settings: Result<Settings, String>,
    audio: bool,
    running: Option<Running>,
    /// Why the agent stopped by itself, if it did.
    exited: Option<String>,
}

struct Running {
    stop: CancellationToken,
    task: JoinHandle<Result<()>>,
    status: watch::Receiver<Status>,
}

impl Agent {
    fn new(dir: &Path) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("can start the async runtime");
        let settings = Identity::load_or_create(dir)
            .and_then(|identity| app::load_settings(dir, identity, Some(Source::Loopback)))
            .map_err(|e| {
                warn!("{e:#}");
                format!("{e:#}")
            });
        Self {
            runtime,
            settings,
            audio: true,
            running: None,
            exited: None,
        }
    }

    fn start(&mut self) {
        let Ok(settings) = &self.settings else { return };
        let mut settings = settings.clone();
        settings.audio = self.audio.then_some(Source::Loopback);
        let (sender, status) = watch::channel(Status::default());
        let stop = CancellationToken::new();
        let task = self
            .runtime
            .spawn(link::run(settings, sender, stop.clone()));
        self.running = Some(Running { stop, task, status });
        self.exited = None;
    }

    /// Stops the agent, giving it a moment to tell the other machine.
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.cancel();
            let _ = self
                .runtime
                .block_on(tokio::time::timeout(Duration::from_secs(3), running.task));
        }
    }

    /// Notices the agent stopping by itself (its port taken, say).
    fn check(&mut self) {
        if !self.running.as_ref().is_some_and(|r| r.task.is_finished()) {
            return;
        }
        let running = self.running.take().expect("checked above");
        let why = match self.runtime.block_on(running.task) {
            Ok(Ok(())) => "the agent stopped".to_string(),
            Ok(Err(e)) => format!("{e:#}"),
            Err(e) => format!("the agent crashed: {e}"),
        };
        error!("{why}");
        self.exited = Some(why);
    }

    fn status(&self) -> Status {
        self.running
            .as_ref()
            .map(|r| r.status.borrow().clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Color {
    Green,
    Amber,
    Red,
    Grey,
}

/// What the tray shows.
#[derive(Clone, Debug, PartialEq)]
struct View {
    color: Color,
    headline: String,
    detail: String,
}

fn view(agent: &Agent) -> View {
    let settings = match &agent.settings {
        Ok(settings) => settings,
        Err(e) => {
            return View {
                color: Color::Amber,
                headline: "Setup needed: see the config folder".into(),
                detail: e.clone(),
            };
        }
    };
    if let Some(why) = &agent.exited {
        return View {
            color: Color::Red,
            headline: "Stopped; quit and start again".into(),
            detail: why.clone(),
        };
    }
    let status = agent.status();
    let Some(peer) = &status.peer else {
        let headline = match &settings.mode {
            Mode::Listen(_) => "Waiting for the PC to connect".to_string(),
            Mode::Connect { host, .. } => format!("Connecting to {host}"),
        };
        return View {
            color: if status.problem.is_some() {
                Color::Amber
            } else {
                Color::Grey
            },
            headline,
            detail: status.problem.unwrap_or_else(|| "Not connected yet".into()),
        };
    };
    let (color, detail) = match &peer.audio {
        AudioStatus::Off if agent.audio => (Color::Green, "Audio isn't available".into()),
        AudioStatus::Off => (Color::Green, "Audio is off".into()),
        AudioStatus::Waiting => (Color::Green, "Audio is starting".into()),
        AudioStatus::Sending { device } => (Color::Green, format!("Sending audio from {device}")),
        AudioStatus::Playing {
            device,
            latency_ms: Some(ms),
        } => (
            Color::Green,
            format!("Playing on {device}, {ms:.0} ms behind"),
        ),
        AudioStatus::Playing { device, .. } => (Color::Green, format!("Playing on {device}")),
        AudioStatus::Failed(why) => (Color::Amber, format!("Audio: {why}")),
    };
    View {
        color,
        headline: format!("Connected to {}", peer.hello.host),
        detail,
    }
}

/// `text`, cut to `max` characters.
fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

/// A filled circle in `color`, for the tray.
fn icon(color: Color) -> Icon {
    const SIZE: u32 = 32;
    let (r, g, b) = match color {
        Color::Green => (0x2e, 0xb8, 0x5c),
        Color::Amber => (0xf0, 0xa2, 0x02),
        Color::Red => (0xd9, 0x30, 0x25),
        Color::Grey => (0x8a, 0x8a, 0x8a),
    };
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0 - 3.0;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let distance = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            // One pixel of soft edge.
            let alpha = (radius + 0.5 - distance).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon")
}

struct Tray {
    icon: TrayIcon,
    headline: MenuItem,
    detail: MenuItem,
    audio: CheckMenuItem,
    open_log: MenuItem,
    open_config: MenuItem,
    login: CheckMenuItem,
    driver: MenuItem,
    quit: MenuItem,
    shown: Option<View>,
}

/// The driver item's text: the Mac's is greyed out, the way "Start at login" is where it isn't
/// supported.
const DRIVER_ITEM: &str = if driver::supported() {
    "Install touchpad driver…"
} else {
    "Install touchpad driver (Windows only)"
};

impl Tray {
    fn new(audio: bool) -> Result<Self> {
        let headline = MenuItem::new("Starting…", false, None);
        let detail = MenuItem::new("", false, None);
        let audio = CheckMenuItem::new("Audio", true, audio, None);
        let open_log = MenuItem::new("Open log", true, None);
        let open_config = MenuItem::new("Open config folder", true, None);
        let login = CheckMenuItem::new(
            "Start at login",
            autostart::supported(),
            autostart::enabled(),
            None,
        );
        let driver = MenuItem::new(DRIVER_ITEM, driver::supported(), None);
        let quit = MenuItem::new("Quit Crossglide", true, None);
        let menu = Menu::new();
        menu.append(&headline)?;
        menu.append(&detail)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&audio)?;
        menu.append(&open_log)?;
        menu.append(&open_config)?;
        menu.append(&login)?;
        menu.append(&driver)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Crossglide")
            .with_icon(icon(Color::Grey))
            .build()?;
        Ok(Self {
            icon,
            headline,
            detail,
            audio,
            open_log,
            open_config,
            login,
            driver,
            quit,
            shown: None,
        })
    }

    fn show(&mut self, view: View) {
        if self.shown.as_ref() == Some(&view) {
            return;
        }
        if self.shown.as_ref().is_none_or(|s| s.color != view.color) {
            let _ = self.icon.set_icon(Some(icon(view.color)));
        }
        self.headline.set_text(short(&view.headline, LINE));
        self.detail.set_text(short(&view.detail, LINE));
        // Windows cuts tooltips at 127 characters.
        let tooltip = short(
            &format!("Crossglide: {}. {}", view.headline, view.detail),
            120,
        );
        let _ = self.icon.set_tooltip(Some(tooltip));
        self.shown = Some(view);
    }
}

/// What wakes the event loop from other threads.
enum UiEvent {
    Menu(MenuEvent),
    /// The Mac's edge hint should show, or go away.
    Hint(Option<Hint>),
    /// The touchpad driver install finished, with this text for its menu item.
    Driver(String),
}

fn run(mut agent: Agent, dir: PathBuf) -> ! {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // A menu bar item only: no Dock icon.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UiEvent::Menu(event));
    }));
    let driver_proxy = proxy.clone();
    let proxy = Mutex::new(proxy);
    touch::on_hint(move |hint| {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(UiEvent::Hint(hint));
        }
    });

    let mut tray: Option<Tray> = None;
    let mut hint_window = HintWindow::default();
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + REFRESH);
        match event {
            // The tray icon can only be made once the event loop runs.
            Event::NewEvents(StartCause::Init) => match Tray::new(agent.audio) {
                Ok(made) => {
                    tray = Some(made);
                    agent.start();
                }
                Err(e) => {
                    error!("can't create the tray icon: {e:#}");
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            },
            Event::UserEvent(UiEvent::Hint(hint)) => hint_window.set(target, hint),
            Event::UserEvent(UiEvent::Driver(text)) => {
                if let Some(tray) = &tray {
                    tray.driver.set_text(text);
                    tray.driver.set_enabled(true);
                }
            }
            Event::UserEvent(UiEvent::Menu(menu)) => {
                let Some(tray) = &tray else { return };
                if menu.id == *tray.audio.id() {
                    agent.audio = tray.audio.is_checked();
                    info!(
                        "audio turned {} in the tray",
                        if agent.audio { "on" } else { "off" }
                    );
                    agent.stop();
                    agent.start();
                } else if menu.id == *tray.open_log.id() {
                    open(&dir.join(LOG_FILE));
                } else if menu.id == *tray.open_config.id() {
                    open(&dir);
                } else if menu.id == *tray.login.id() {
                    let on = tray.login.is_checked();
                    match autostart::set(on) {
                        Ok(()) => info!("start at login turned {}", if on { "on" } else { "off" }),
                        Err(e) => {
                            error!("{e:#}");
                            tray.login.set_checked(!on);
                        }
                    }
                } else if menu.id == *tray.driver.id() {
                    // Off until it's over, so a second click can't start a second install.
                    tray.driver.set_enabled(false);
                    info!("installing the touchpad driver");
                    let proxy = driver_proxy.clone();
                    driver::start(move |result| {
                        let text = match result {
                            Ok(()) => "Touchpad driver installed (install again)".to_string(),
                            Err(e) => {
                                error!("touchpad driver: {e:#}");
                                format!("Install touchpad driver… (failed: {e})")
                            }
                        };
                        let _ = proxy.send_event(UiEvent::Driver(short(&text, LINE)));
                    });
                } else if menu.id == *tray.quit.id() {
                    info!("quitting");
                    agent.stop();
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            _ => {}
        }
        agent.check();
        if let Some(tray) = &mut tray {
            tray.show(view(&agent));
        }
    })
}

/// Opens a file or folder the way the desktop would.
fn open(path: &Path) {
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = Command::new(program).arg(path).spawn() {
        warn!("can't open {}: {e}", path.display());
    }
}
