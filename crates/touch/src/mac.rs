//! The Mac side: raw trackpad contacts from the private MultitouchSupport framework, and a
//! Quartz event tap. The tap watches for the hotkey and for the pointer being pushed through a
//! screen edge; while the PC has control it swallows the Mac's own input, passing keys and
//! clicks on instead.
//!
//! The tap needs Accessibility permission for the app that runs crossglide (Terminal, when it
//! runs from source).

use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::edge::{self, Edge, Rect};
use crate::frame::{Contact, Finger, Tracker};
use crate::keymap::{Hotkey, modifier};

// --- MultitouchSupport ---------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct MtPoint {
    x: f32,
    y: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MtReadout {
    pos: MtPoint,
    vel: MtPoint,
}

/// One finger, as the framework lays it out (reverse-engineered; stable for many releases).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct MtFinger {
    frame: c_int,
    timestamp: f64,
    identifier: c_int,
    state: c_int,
    finger_id: c_int,
    hand_id: c_int,
    /// Position in fractions of the surface, from the bottom left.
    normalized: MtReadout,
    size: f32,
    zero1: c_int,
    angle: f32,
    major_axis: f32,
    minor_axis: f32,
    mm: MtReadout,
    zero2: [c_int; 2],
    unk2: f32,
}

/// `MTTouchState` values for a finger pressing the surface.
const MAKE_TOUCH: c_int = 3;
const TOUCHING: c_int = 4;

type MtDevice = *mut c_void;
type MtCallback = extern "C" fn(MtDevice, *const MtFinger, c_int, f64, c_int) -> c_int;

#[link(name = "MultitouchSupport", kind = "framework")]
unsafe extern "C" {
    fn MTDeviceCreateList() -> *const c_void;
    fn MTDeviceIsBuiltIn(device: MtDevice) -> bool;
    fn MTRegisterContactFrameCallback(device: MtDevice, callback: MtCallback);
    fn MTDeviceStart(device: MtDevice, mode: c_int);
}

/// Receives each frame's contacts and its time in 100 µs, on the framework's thread.
type Sink = Box<dyn FnMut(Vec<Contact>, u32) + Send>;

struct CaptureState {
    tracker: Tracker,
    sink: Sink,
}

static CAPTURE: Mutex<Option<CaptureState>> = Mutex::new(None);

extern "C" fn on_frame(
    _device: MtDevice,
    fingers: *const MtFinger,
    count: c_int,
    timestamp: f64,
    _frame: c_int,
) -> c_int {
    let fingers = if fingers.is_null() || count <= 0 {
        &[][..]
    } else {
        // SAFETY: the framework passes `count` fingers, valid for this call.
        unsafe { std::slice::from_raw_parts(fingers, count as usize) }
    };
    let fingers: Vec<Finger> = fingers
        .iter()
        .map(|f| Finger {
            identifier: f.identifier,
            touching: matches!(f.state, MAKE_TOUCH | TOUCHING),
            x: f.normalized.pos.x,
            y: 1.0 - f.normalized.pos.y,
        })
        .collect();
    if let Ok(mut state) = CAPTURE.lock()
        && let Some(state) = state.as_mut()
        && let Some(contacts) = state.tracker.update(&fingers)
    {
        (state.sink)(contacts, (timestamp * 10_000.0) as u64 as u32);
    }
    0
}

/// Starts reporting the built-in trackpad's contacts to `sink` (on the framework's own thread)
/// for as long as the process runs. Only one capture runs; a second call fails.
pub fn start_capture(sink: impl FnMut(Vec<Contact>, u32) + Send + 'static) -> Result<(), String> {
    let mut state = CAPTURE.lock().map_err(|_| "the trackpad capture crashed")?;
    if state.is_some() {
        return Err("the trackpad capture is already running".into());
    }
    // SAFETY: plain calls into the framework; the list is never released, so neither are the
    // devices in it.
    let device = unsafe {
        let list = MTDeviceCreateList();
        let count = if list.is_null() {
            0
        } else {
            CFArrayGetCount(list)
        };
        let devices: Vec<MtDevice> = (0..count)
            .map(|i| CFArrayGetValueAtIndex(list, i) as MtDevice)
            .collect();
        devices
            .iter()
            .copied()
            .find(|&d| MTDeviceIsBuiltIn(d))
            .or(devices.first().copied())
    };
    let Some(device) = device else {
        return Err("this Mac has no trackpad".into());
    };
    *state = Some(CaptureState {
        tracker: Tracker::default(),
        sink: Box::new(sink),
    });
    drop(state);
    // SAFETY: `device` came from the framework's list, which is never released.
    unsafe {
        MTRegisterContactFrameCallback(device, on_frame);
        MTDeviceStart(device, 0);
    }
    Ok(())
}

// --- Quartz --------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CgPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CgRect {
    origin: CgPoint,
    size: CgPoint,
}

type CgEvent = *mut c_void;
type CfMachPort = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, CgEvent, *mut c_void) -> CgEvent;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CfMachPort,
        order: isize,
    ) -> *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRelease(object: *const c_void);
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    static kCFRunLoopCommonModes: *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CfMachPort;
    fn CGEventTapEnable(tap: CfMachPort, enable: bool);
    fn CGEventCreate(source: *const c_void) -> CgEvent;
    fn CGEventGetLocation(event: CgEvent) -> CgPoint;
    fn CGEventGetFlags(event: CgEvent) -> u64;
    fn CGEventGetIntegerValueField(event: CgEvent, field: u32) -> i64;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CgRect;
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGWarpMouseCursorPosition(point: CgPoint) -> i32;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const DEFAULT_OPTIONS: u32 = 0;
const ALL_EVENTS: u64 = !0;

mod event {
    pub const LEFT_DOWN: u32 = 1;
    pub const LEFT_UP: u32 = 2;
    pub const RIGHT_DOWN: u32 = 3;
    pub const RIGHT_UP: u32 = 4;
    pub const MOVED: u32 = 5;
    pub const LEFT_DRAGGED: u32 = 6;
    pub const RIGHT_DRAGGED: u32 = 7;
    pub const KEY_DOWN: u32 = 10;
    pub const KEY_UP: u32 = 11;
    pub const FLAGS_CHANGED: u32 = 12;
    pub const OTHER_DOWN: u32 = 25;
    pub const OTHER_UP: u32 = 26;
    pub const OTHER_DRAGGED: u32 = 27;
    pub const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    pub const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
}

mod field {
    pub const MOUSE_DELTA_X: u32 = 4;
    pub const MOUSE_DELTA_Y: u32 = 5;
    pub const KEY_AUTOREPEAT: u32 = 8;
    pub const KEY_CODE: u32 = 9;
}

mod flag {
    pub const SHIFT: u64 = 0x2_0000;
    pub const CONTROL: u64 = 0x4_0000;
    pub const OPTION: u64 = 0x8_0000;
    pub const COMMAND: u64 = 0x10_0000;
}

/// Mac key code of Caps Lock.
const CAPS_LOCK: u16 = 0x39;

/// Whether this process may tap input. With `prompt`, macOS shows its dialog pointing at the
/// Accessibility settings if it may not.
pub fn accessibility_trusted(prompt: bool) -> bool {
    // SAFETY: a one-entry CFDictionary of framework constants, released after the call.
    unsafe {
        if !prompt {
            return AXIsProcessTrustedWithOptions(ptr::null());
        }
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        CFRelease(options);
        trusted
    }
}

/// The active displays, in global coordinates (y down, origin at the main display's top left).
pub fn displays() -> Vec<Rect> {
    let mut ids = [0u32; 16];
    let mut count = 0;
    // SAFETY: the buffer holds 16 IDs and Quartz writes at most that many.
    if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    ids[..count as usize]
        .iter()
        // SAFETY: IDs from the list above.
        .map(|&id| unsafe { CGDisplayBounds(id) })
        .map(|r| Rect {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.x,
            h: r.size.y,
        })
        .collect()
}

/// Where the pointer is.
pub fn pointer() -> (f64, f64) {
    // SAFETY: an event from no source just carries the current location; released after use.
    unsafe {
        let event = CGEventCreate(ptr::null());
        if event.is_null() {
            return (0.0, 0.0);
        }
        let at = CGEventGetLocation(event);
        CFRelease(event);
        (at.x, at.y)
    }
}

/// Stops the pointer following the mouse and trackpad (while the PC has control), or lets it
/// follow again.
pub fn freeze_pointer(frozen: bool) {
    // SAFETY: a plain Quartz call.
    unsafe { CGAssociateMouseAndMouseCursorPosition(u32::from(!frozen)) };
}

/// Puts the pointer just inside `edge`, `along` the way, as control comes back to the Mac.
pub fn place_pointer(edge: Edge, along: f64) {
    if let Some((x, y)) = edge::entry_point(&displays(), edge, along) {
        // SAFETY: a plain Quartz call.
        unsafe { CGWarpMouseCursorPosition(CgPoint { x, y }) };
    }
}

/// What the tap reports, on its own thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TapEvent {
    /// The hotkey was pressed.
    Hotkey,
    /// While the Mac has control: the pointer was pushed through `edge` of `display`, `along`
    /// the way.
    Edge {
        edge: Edge,
        along: f64,
        display: Rect,
    },
    /// While the PC has control: a key went down or up (Mac key code).
    Key { code: u16, down: bool },
}

/// State the tap shares with the agent.
#[derive(Debug, Default)]
pub struct Shared {
    /// The PC has control: swallow the Mac's input.
    pub active: AtomicBool,
    /// The trackpad (or a mouse button) is pressed, while the PC has control.
    pub button: AtomicBool,
    /// An edge push was reported and the agent hasn't dealt with it yet; set by the tap,
    /// cleared by the agent.
    pub switching: AtomicBool,
}

struct Tap {
    port: CfMachPort,
    hotkey: Hotkey,
    edges: Vec<Edge>,
    shared: Arc<Shared>,
    events: Box<dyn Fn(TapEvent) + Send>,
    /// The hotkey's key is down; its repeats and release are swallowed too.
    hotkey_down: bool,
    displays: Vec<Rect>,
    displays_read: Option<Instant>,
}

/// How long the display layout is trusted before it's read again.
const DISPLAYS_FOR: Duration = Duration::from_secs(1);

/// Device-dependent modifier bits (`NX_DEVICE*KEYMASK`): which of a left/right pair is down.
fn modifier_bit(code: u16) -> Option<u64> {
    Some(match code {
        0x3B => 0x0001, // left Control
        0x38 => 0x0002, // left Shift
        0x3C => 0x0004, // right Shift
        0x37 => 0x0008, // left Command
        0x36 => 0x0010, // right Command
        0x3A => 0x0020, // left Option
        0x3D => 0x0040, // right Option
        0x3E => 0x2000, // right Control
        _ => return None,
    })
}

fn modifiers(flags: u64) -> u8 {
    [
        (flag::SHIFT, modifier::SHIFT),
        (flag::CONTROL, modifier::CONTROL),
        (flag::OPTION, modifier::OPTION),
        (flag::COMMAND, modifier::COMMAND),
    ]
    .iter()
    .filter(|(f, _)| flags & f != 0)
    .fold(0, |m, (_, bit)| m | bit)
}

impl Tap {
    /// Decides one event: `true` passes it on, `false` swallows it.
    fn handle(&mut self, kind: u32, event: CgEvent) -> bool {
        let active = self.shared.active.load(Relaxed);
        // SAFETY (every Quartz call below): `event` is this callback's live event.
        match kind {
            event::TAP_DISABLED_BY_TIMEOUT | event::TAP_DISABLED_BY_USER_INPUT => {
                unsafe { CGEventTapEnable(self.port, true) };
                return true;
            }
            event::KEY_DOWN | event::KEY_UP => {
                let code = unsafe { CGEventGetIntegerValueField(event, field::KEY_CODE) } as u16;
                let down = kind == event::KEY_DOWN;
                if code == self.hotkey.key {
                    let repeat =
                        unsafe { CGEventGetIntegerValueField(event, field::KEY_AUTOREPEAT) } != 0;
                    let flags = unsafe { CGEventGetFlags(event) };
                    if down && !repeat && modifiers(flags) == self.hotkey.modifiers {
                        self.hotkey_down = true;
                        (self.events)(TapEvent::Hotkey);
                        return false;
                    }
                    if self.hotkey_down {
                        self.hotkey_down = down;
                        return false;
                    }
                }
                if active {
                    (self.events)(TapEvent::Key { code, down });
                }
            }
            event::FLAGS_CHANGED if active => {
                let code = unsafe { CGEventGetIntegerValueField(event, field::KEY_CODE) } as u16;
                let flags = unsafe { CGEventGetFlags(event) };
                if let Some(bit) = modifier_bit(code) {
                    let down = flags & bit != 0;
                    (self.events)(TapEvent::Key { code, down });
                } else if code == CAPS_LOCK {
                    // The Mac reports each toggle; a press and release toggles the PC's.
                    (self.events)(TapEvent::Key { code, down: true });
                    (self.events)(TapEvent::Key { code, down: false });
                }
            }
            event::LEFT_DOWN | event::RIGHT_DOWN | event::OTHER_DOWN if active => {
                self.shared.button.store(true, Relaxed);
            }
            event::LEFT_UP | event::RIGHT_UP | event::OTHER_UP if active => {
                self.shared.button.store(false, Relaxed);
            }
            event::MOVED | event::LEFT_DRAGGED | event::RIGHT_DRAGGED | event::OTHER_DRAGGED
                if !active =>
            {
                let at = unsafe { CGEventGetLocation(event) };
                let dx = unsafe { CGEventGetIntegerValueField(event, field::MOUSE_DELTA_X) };
                let dy = unsafe { CGEventGetIntegerValueField(event, field::MOUSE_DELTA_Y) };
                self.check_edges(at, dx as f64, dy as f64);
            }
            _ => {}
        }
        // While the PC has control nothing reaches the Mac: not keys and clicks (sent on above),
        // nor moves, scrolls and gestures (the trackpad's contacts already reach the PC).
        !active
    }

    fn check_edges(&mut self, at: CgPoint, dx: f64, dy: f64) {
        if self.edges.is_empty() || self.shared.switching.load(Relaxed) {
            return;
        }
        if self
            .displays_read
            .is_none_or(|read| read.elapsed() > DISPLAYS_FOR)
        {
            self.displays = displays();
            self.displays_read = Some(Instant::now());
        }
        for &edge in &self.edges {
            if edge.toward(dx, dy) > 0.0
                && let Some(along) = edge::at_edge(&self.displays, at.x, at.y, edge)
                && let Some(display) = edge::display_at(&self.displays, at.x, at.y)
            {
                self.shared.switching.store(true, Relaxed);
                (self.events)(TapEvent::Edge {
                    edge,
                    along,
                    display,
                });
                return;
            }
        }
    }
}

extern "C" fn on_event(
    _proxy: *mut c_void,
    kind: u32,
    event: CgEvent,
    user_info: *mut c_void,
) -> CgEvent {
    // SAFETY: `user_info` is the leaked `Tap`, only ever used on the tap's run loop thread.
    let tap = unsafe { &mut *(user_info as *mut Tap) };
    if tap.handle(kind, event) {
        event
    } else {
        ptr::null_mut()
    }
}

/// Starts the event tap on a thread of its own, for as long as the process runs. Fails without
/// Accessibility permission.
pub fn start_tap(
    hotkey: Hotkey,
    edges: Vec<Edge>,
    shared: Arc<Shared>,
    events: impl Fn(TapEvent) + Send + 'static,
) -> Result<(), String> {
    let (started, result) = mpsc::channel();
    let events: Box<dyn Fn(TapEvent) + Send> = Box::new(events);
    thread::Builder::new()
        .name("crossglide-tap".into())
        .spawn(move || {
            let tap = Box::into_raw(Box::new(Tap {
                port: ptr::null_mut(),
                hotkey,
                edges,
                shared,
                events,
                hotkey_down: false,
                displays: Vec::new(),
                displays_read: None,
            }));
            // SAFETY: the tap state is leaked for the life of the thread (the process); the
            // callback only runs on this thread's run loop.
            unsafe {
                let port = CGEventTapCreate(
                    SESSION_TAP,
                    HEAD_INSERT,
                    DEFAULT_OPTIONS,
                    ALL_EVENTS,
                    on_event,
                    tap.cast(),
                );
                if port.is_null() {
                    drop(Box::from_raw(tap));
                    let _ = started.send(Err("can't tap input: give the app running crossglide \
                         (Terminal, when run from source) Accessibility permission in System \
                         Settings → Privacy & Security, then restart it"
                        .to_string()));
                    return;
                }
                (*tap).port = port;
                let source = CFMachPortCreateRunLoopSource(ptr::null(), port, 0);
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CGEventTapEnable(port, true);
                let _ = started.send(Ok(()));
                CFRunLoopRun();
            }
        })
        .map_err(|e| format!("can't start the input tap: {e}"))?;
    result
        .recv()
        .unwrap_or_else(|_| Err("the input tap thread stopped".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_finger_layout_matches_the_framework() {
        assert_eq!(std::mem::size_of::<MtFinger>(), 96);
    }

    #[test]
    fn modifier_flags_become_hotkey_bits() {
        let caps_lock = 0x1_0000;
        assert_eq!(
            modifiers(flag::CONTROL | flag::COMMAND | caps_lock | 0x0108),
            modifier::CONTROL | modifier::COMMAND
        );
    }
}
