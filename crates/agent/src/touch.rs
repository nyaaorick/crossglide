//! Touch over the side channel: while the PC has control, the Mac's trackpad contacts go to the
//! PC as QUIC datagrams and drive its virtual precision touchpad, and the Mac's keys go over the
//! control stream. The Mac decides who has control: pushing the pointer through one of its
//! `[touch] edges` or pressing the hotkey moves control to the PC; pushing through the PC's
//! opposite edge, or the hotkey again, brings it back.

use std::sync::OnceLock;

use crossglide_touch::edge::{Edge, Rect};
use crossglide_touch::keymap::{CommandKey, Hotkey};
use quinn::Connection;
use tokio::sync::mpsc;

use crate::control::{Control, Handled};
use crate::proto::{Request, Response};

/// Listed in the hello by agents that can do touch: the Mac's capture, or the PC's injection.
pub const FEATURE: &str = "touch";

/// What the Mac shows while the PC has control: a strip over `edge` of `display`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hint {
    pub edge: Edge,
    pub display: Rect,
}

type HintObserver = Box<dyn Fn(Option<Hint>) + Send + Sync>;

static HINT: OnceLock<HintObserver> = OnceLock::new();

/// Sets what's told when the hint should show (`Some`) or go away (`None`), from any thread.
/// Only the tray app has a window to show it in; without an observer the hint is skipped.
pub fn on_hint(observer: impl Fn(Option<Hint>) + Send + Sync + 'static) {
    let _ = HINT.set(Box::new(observer));
}

/// The Mac's `[touch]` settings.
#[derive(Clone, Debug, PartialEq)]
pub struct TouchSettings {
    pub edges: Vec<Edge>,
    pub hotkey: Hotkey,
    pub command: CommandKey,
}

/// One connection's touch, on either side.
pub struct Session {
    pub conn: Connection,
    pub control: Control,
    pub peer: String,
}

/// Whether `request` is for touch rather than audio.
pub fn handles(request: &Request) -> bool {
    matches!(
        request,
        Request::TouchEnter { .. }
            | Request::TouchLeave
            | Request::TouchReturn { .. }
            | Request::Key { .. }
    )
}

/// On the Mac: starts the trackpad capture and input tap now, so a missing permission shows up
/// at startup rather than at the first connection.
pub fn prepare(settings: &TouchSettings) {
    #[cfg(target_os = "macos")]
    if let Err(e) = mac::Local::get(settings) {
        tracing::warn!("touch: {e}");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = settings;
}

/// Runs touch for one connection until its control stream closes: the Mac's side when
/// `listening`, the PC's otherwise. `settings` is the Mac's `[touch]`, `None` when off.
pub async fn serve(
    session: Session,
    settings: Option<&TouchSettings>,
    listening: bool,
    requests: mpsc::Receiver<Handled>,
) {
    if listening {
        #[cfg(target_os = "macos")]
        if let Some(settings) = settings {
            // Already logged by `prepare` if it failed.
            match mac::Local::get(settings) {
                Ok(local) => return mac::serve(local, session, requests).await,
                Err(e) => return idle(requests, &e).await,
            }
        }
        let _ = (settings, session);
        return idle(requests, "touch is off on the Mac").await;
    }
    #[cfg(windows)]
    return pc::serve(session, requests).await;
    #[cfg(not(windows))]
    {
        let _ = session;
        idle(requests, "only a Windows PC can take touch input").await
    }
}

/// Answers touch requests with an error until the control stream closes.
pub async fn idle(mut requests: mpsc::Receiver<Handled>, why: &str) {
    while let Some((_, reply)) = requests.recv().await {
        let _ = reply.send(Response::Error {
            message: why.to_string(),
        });
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::AtomicU16;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use crossglide_touch::edge::{self, Edge};
    use crossglide_touch::frame::Frame;
    use crossglide_touch::keymap;
    use crossglide_touch::mac::{self, Shared, TapEvent};
    use quinn::Connection;
    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use tracing::{info, warn};

    use super::{HINT, Hint, Session, TouchSettings};
    use crate::control::{Handled, Reply};
    use crate::proto::{Request, Response};

    /// How long the PC gets to take control before the Mac keeps it.
    const ENTER_TIMEOUT: Duration = Duration::from_secs(1);

    /// The Mac's trackpad capture and input tap. They start once and run for the life of the
    /// process, across connections (and agent restarts from the tray); each connection's
    /// session takes their events in turn.
    pub struct Local {
        settings: TouchSettings,
        shared: Arc<Shared>,
        /// Where frames go while the PC has control.
        conn: Arc<Mutex<Option<Connection>>>,
        events: tokio::sync::Mutex<mpsc::UnboundedReceiver<TapEvent>>,
    }

    static LOCAL: OnceLock<Result<Arc<Local>, String>> = OnceLock::new();

    impl Local {
        /// Starts the capture and tap the first time; later calls return the same ones, with
        /// the first call's settings (changed settings need a restart).
        pub fn get(settings: &TouchSettings) -> Result<Arc<Self>, String> {
            LOCAL
                .get_or_init(|| Self::start(settings.clone()).map(Arc::new))
                .clone()
        }

        fn start(settings: TouchSettings) -> Result<Self, String> {
            if !mac::accessibility_trusted(true) {
                return Err(
                    "the app running crossglide (Terminal, when run from source) \
                            needs Accessibility permission for the trackpad and keyboard to \
                            control the PC: allow it in System Settings → Privacy & Security → \
                            Accessibility, then restart crossglide"
                        .into(),
                );
            }
            let shared = Arc::new(Shared::default());
            let conn: Arc<Mutex<Option<Connection>>> = Arc::default();
            let (events, receiver) = mpsc::unbounded_channel();
            mac::start_tap(
                settings.hotkey,
                settings.edges.clone(),
                shared.clone(),
                move |event| {
                    let _ = events.send(event);
                },
            )?;
            let seq = AtomicU16::new(0);
            mac::start_capture({
                let shared = shared.clone();
                let conn = conn.clone();
                move |contacts, time| {
                    if !shared.active.load(Relaxed) {
                        return;
                    }
                    let frame = Frame {
                        seq: seq.fetch_add(1, Relaxed).wrapping_add(1),
                        time,
                        button: shared.button.load(Relaxed),
                        contacts,
                    };
                    // Straight from the trackpad's thread to the network: no queue to wait in.
                    if let Ok(conn) = conn.lock()
                        && let Some(conn) = conn.as_ref()
                    {
                        let _ = conn.send_datagram(frame.encode().into());
                    }
                }
            })?;
            info!(
                "touch: push the pointer through the {} side of the screen, or press the \
                 hotkey, to control the PC",
                sides(&settings.edges)
            );
            Ok(Self {
                settings,
                shared,
                conn,
                events: tokio::sync::Mutex::new(receiver),
            })
        }

        /// Hands control to the PC: the Mac's input stops reaching it and goes to the PC, and
        /// `hint` covers the edge it left through.
        fn activate(&self, conn: &Connection, hint: Option<Hint>) {
            *self.conn.lock().expect("never poisoned") = Some(conn.clone());
            self.shared.button.store(false, Relaxed);
            self.shared.active.store(true, Relaxed);
            mac::freeze_pointer(true);
            show_hint(hint);
        }

        /// Takes control back. Every way back comes through here, so the hint can't outlive it.
        fn deactivate(&self) {
            self.shared.active.store(false, Relaxed);
            self.shared.button.store(false, Relaxed);
            mac::freeze_pointer(false);
            *self.conn.lock().expect("never poisoned") = None;
            show_hint(None);
        }

        /// The hint for entering by the hotkey, which has no exit edge: the first of the
        /// configured edges, on the display the pointer is on.
        fn hotkey_hint(&self) -> Option<Hint> {
            let edge = *self.settings.edges.first()?;
            let displays = mac::displays();
            let (x, y) = mac::pointer();
            let display =
                edge::display_at(&displays, x, y).or_else(|| displays.first().copied())?;
            Some(Hint { edge, display })
        }
    }

    fn show_hint(hint: Option<Hint>) {
        if let Some(observer) = HINT.get() {
            observer(hint);
        }
    }

    fn sides(edges: &[Edge]) -> String {
        if edges.is_empty() {
            return "no".into();
        }
        edges
            .iter()
            .map(|e| format!("{e:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// Runs touch for one connection until its control stream closes.
    pub async fn serve(local: Arc<Local>, session: Session, mut requests: mpsc::Receiver<Handled>) {
        let mut events = local.events.lock().await;
        // Events from before this connection (or between connections) are stale.
        while events.try_recv().is_ok() {}
        local.shared.switching.store(false, Relaxed);
        let mut on_pc = false;
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { break };
                    on_pc = handle(&local, &session, event, on_pc).await;
                }
                request = requests.recv() => {
                    let Some((request, reply)) = request else { break };
                    let response = match request {
                        Request::TouchReturn { edge, along } => {
                            if on_pc {
                                local.deactivate();
                                on_pc = false;
                                mac::place_pointer(edge.opposite(), along);
                                info!("touch: control is back on the Mac");
                            }
                            Response::Ok
                        }
                        _ => Response::Error {
                            message: "the Mac doesn't take that request".into(),
                        },
                    };
                    let _ = reply.send(response);
                }
            }
        }
        if on_pc {
            local.deactivate();
            info!(
                "touch: control is back on the Mac: the connection to {} closed",
                session.peer
            );
        }
    }

    /// Handles one tap event; returns whether the PC has control afterwards.
    async fn handle(local: &Local, session: &Session, event: TapEvent, on_pc: bool) -> bool {
        match event {
            TapEvent::Hotkey if on_pc => {
                local.deactivate();
                let _ = session.control.send(Request::TouchLeave).await;
                info!("touch: control is back on the Mac (hotkey)");
                false
            }
            TapEvent::Hotkey => enter(local, session, None, local.hotkey_hint()).await,
            TapEvent::Edge {
                edge,
                along,
                display,
            } => {
                let entry = Some((edge.opposite(), along));
                let on_pc =
                    on_pc || enter(local, session, entry, Some(Hint { edge, display })).await;
                local.shared.switching.store(false, Relaxed);
                on_pc
            }
            TapEvent::Key { code, down } => {
                if on_pc && let Some(code) = keymap::scancode(code, local.settings.command) {
                    let _ = session.control.send(Request::Key { code, down }).await;
                }
                on_pc
            }
        }
    }

    /// Asks the PC to take control; returns whether it did. `hint` is shown while it has it.
    async fn enter(
        local: &Local,
        session: &Session,
        entry: Option<(Edge, f64)>,
        hint: Option<Hint>,
    ) -> bool {
        let returns = local.settings.edges.iter().map(|e| e.opposite()).collect();
        let request = Request::TouchEnter { entry, returns };
        match timeout(ENTER_TIMEOUT, session.control.request(request)).await {
            Ok(Ok(Reply {
                body: Response::Ok, ..
            })) => {
                local.activate(&session.conn, hint);
                info!("touch: {} has control", session.peer);
                true
            }
            Ok(Ok(Reply {
                body: Response::Error { message },
                ..
            })) => {
                warn!("touch: {} can't take control: {message}", session.peer);
                false
            }
            Ok(Ok(_)) => false,
            Ok(Err(e)) => {
                warn!("touch: {e:#}");
                false
            }
            Err(_) => {
                warn!(
                    "touch: {} didn't take control within {ENTER_TIMEOUT:?}",
                    session.peer
                );
                false
            }
        }
    }
}

#[cfg(windows)]
mod pc {
    use std::sync::mpsc as std_mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use crossglide_touch::edge::{Edge, Rect, ReturnDetector};
    use crossglide_touch::frame::{self, Frame};
    use crossglide_touch::report;
    use crossglide_touch::win::{self, Keyboard, Touchpad};
    use tokio::sync::mpsc;
    use tracing::{info, warn};

    use super::Session;
    use crate::audio::AbortOnDrop;
    use crate::control::Handled;
    use crate::proto::{Request, Response};

    /// With fingers down and no frame for this long, the Mac is gone or its last frames were
    /// lost: lift everything.
    const STALE: Duration = Duration::from_millis(250);
    /// How long the monitor layout is trusted before it's read again.
    const DISPLAYS_FOR: Duration = Duration::from_secs(1);

    enum Input {
        Frame(Frame),
        Enter {
            entry: Option<(Edge, f64)>,
            returns: Vec<Edge>,
        },
        Leave,
        Key {
            code: u16,
            down: bool,
        },
    }

    /// Runs touch for one connection until its control stream closes.
    pub async fn serve(session: Session, mut requests: mpsc::Receiver<Handled>) {
        let touchpad = match Touchpad::open() {
            Ok(touchpad) => Some(touchpad),
            Err(e) => {
                warn!("touch: {e}; the Mac's trackpad can't control this PC");
                None
            }
        };
        let unavailable = touchpad.is_none().then(|| {
            "the PC's virtual touchpad isn't installed (tray menu: Install touchpad driver)"
                .to_string()
        });
        let (inputs, received) = std_mpsc::channel();
        let (returns, mut returned) = mpsc::unbounded_channel();
        thread::Builder::new()
            .name("crossglide-touch".into())
            .spawn(move || inject(touchpad, received, returns))
            .expect("can start a thread");

        let _reader = AbortOnDrop(tokio::spawn({
            let conn = session.conn.clone();
            let inputs = inputs.clone();
            async move {
                let mut refused = false;
                while let Ok(datagram) = conn.read_datagram().await {
                    match Frame::decode(&datagram) {
                        Ok(frame) => {
                            if inputs.send(Input::Frame(frame)).is_err() {
                                break;
                            }
                        }
                        Err(e) if !refused => {
                            warn!("touch: dropping datagrams from the Mac: {e}");
                            refused = true;
                        }
                        Err(_) => {}
                    }
                }
            }
        }));

        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some((request, reply)) = request else { break };
                    let input = match request {
                        Request::TouchEnter { .. } if unavailable.is_some() => {
                            let message = unavailable.clone().expect("checked above");
                            let _ = reply.send(Response::Error { message });
                            continue;
                        }
                        Request::TouchEnter { entry, returns } => {
                            info!("touch: {} gave this PC control", session.peer);
                            Input::Enter { entry, returns }
                        }
                        Request::TouchLeave => {
                            info!("touch: control is back on {}", session.peer);
                            Input::Leave
                        }
                        Request::Key { code, down } => Input::Key { code, down },
                        _ => {
                            let message = "the PC doesn't take that request".into();
                            let _ = reply.send(Response::Error { message });
                            continue;
                        }
                    };
                    let _ = inputs.send(input);
                    let _ = reply.send(Response::Ok);
                }
                Some((edge, along)) = returned.recv() => {
                    info!(
                        "touch: pushed through the {edge:?} edge; control is back on {}",
                        session.peer
                    );
                    let _ = session.control.send(Request::TouchReturn { edge, along }).await;
                }
            }
        }
        // Dropping `inputs` (the reader's copy goes with it) ends the injector, which lets go
        // of everything still pressed.
    }

    /// The injector thread: writes frames to the touchpad and types keys while the PC has
    /// control, and notices the pointer pushed back through an edge.
    fn inject(
        touchpad: Option<Touchpad>,
        inputs: std_mpsc::Receiver<Input>,
        returns: mpsc::UnboundedSender<(Edge, f64)>,
    ) {
        win::per_monitor_dpi();
        let mut injector = Injector {
            touchpad,
            keyboard: Keyboard::default(),
            active: false,
            detector: ReturnDetector::default(),
            last: None,
            last_seq: None,
            displays: Vec::new(),
            displays_read: None,
            write_failed: false,
            key_failed: false,
        };
        loop {
            match inputs.recv_timeout(STALE) {
                Ok(Input::Frame(frame)) => {
                    if let Some(back) = injector.frame(frame) {
                        let _ = returns.send(back);
                    }
                }
                Ok(Input::Enter { entry, returns }) => injector.enter(entry, returns),
                Ok(Input::Leave) => injector.leave(),
                Ok(Input::Key { code, down }) => injector.key(code, down),
                Err(std_mpsc::RecvTimeoutError::Timeout) => injector.lift(),
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        injector.leave();
    }

    struct Injector {
        touchpad: Option<Touchpad>,
        keyboard: Keyboard,
        active: bool,
        detector: ReturnDetector,
        /// The last frame written, while it has fingers down or the button pressed.
        last: Option<Frame>,
        last_seq: Option<u16>,
        displays: Vec<Rect>,
        displays_read: Option<Instant>,
        write_failed: bool,
        key_failed: bool,
    }

    impl Injector {
        fn enter(&mut self, entry: Option<(Edge, f64)>, returns: Vec<Edge>) {
            self.active = true;
            self.detector = ReturnDetector::new(returns);
            self.last_seq = None;
            if let Some((edge, along)) = entry {
                win::place_pointer(edge, along);
            }
        }

        fn leave(&mut self) {
            self.active = false;
            self.lift();
            self.keyboard.release_all();
        }

        /// Writes one frame; returns the edge and position if it pushed the pointer back to
        /// the Mac.
        fn frame(&mut self, frame: Frame) -> Option<(Edge, f64)> {
            let late = self
                .last_seq
                .is_some_and(|last| !frame::is_newer(frame.seq, last));
            if !self.active || late {
                return None;
            }
            self.last_seq = Some(frame.seq);
            self.write(&frame);
            if self
                .displays_read
                .is_none_or(|read| read.elapsed() > DISPLAYS_FOR)
            {
                self.displays = win::displays();
                self.displays_read = Some(Instant::now());
            }
            let displays = &self.displays;
            let back = self
                .detector
                .update(&frame, |edge| win::pointer_at_edge(displays, edge));
            self.last = (frame.touching() || frame.button).then_some(frame);
            if back.is_some() {
                self.leave();
            }
            back
        }

        /// Lets go of the last frame's fingers and button, if any are still down.
        fn lift(&mut self) {
            if let Some(last) = self.last.take() {
                self.write(&last.lifted());
            }
        }

        fn write(&mut self, frame: &Frame) {
            let Some(touchpad) = &self.touchpad else {
                return;
            };
            match touchpad.send(&report::touchpad(frame)) {
                Ok(()) => self.write_failed = false,
                Err(e) if !self.write_failed => {
                    warn!("touch: can't write to the virtual touchpad: {e}");
                    self.write_failed = true;
                }
                Err(_) => {}
            }
        }

        fn key(&mut self, code: u16, down: bool) {
            if !self.active {
                return;
            }
            match self.keyboard.key(code, down) {
                Ok(()) => self.key_failed = false,
                Err(e) if !self.key_failed => {
                    warn!(
                        "touch: Windows refused a key ({e}); keys don't reach apps running as \
                         administrator unless crossglide does too"
                    );
                    self.key_failed = true;
                }
                Err(_) => {}
            }
        }
    }
}
