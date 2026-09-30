//! The PC side: writes touchpad reports to the virtual touchpad in `drivers/touchpad`, types the
//! Mac's keys, and finds and moves the pointer at the screen's edges.

use std::collections::HashSet;
use std::ffi::c_void;
use std::io;
use std::ptr;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW,
    CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetAttributes,
    HidD_GetHidGuid, HidD_GetPreparsedData, HidP_GetCaps,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LPARAM, POINT, RECT,
};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, WriteFile,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

use crate::edge::{self, Edge, Rect};
use crate::keymap::EXTENDED;
use crate::report::FEED_REPORT_LEN;

/// The virtual touchpad's USB IDs, as its driver reports them.
const VENDOR_ID: u16 = 0x1209;
const PRODUCT_ID: u16 = 0xC6D0;
/// Usage page and usage of its feed collection.
const FEED_USAGE_PAGE: u16 = 0xFF42;
const FEED_USAGE: u16 = 0x01;

/// The virtual touchpad's feed collection, open for writing.
pub struct Touchpad {
    handle: HANDLE,
}

// SAFETY: a file handle; WriteFile on it is fine from any thread.
unsafe impl Send for Touchpad {}

impl Touchpad {
    /// Finds the virtual touchpad. Fails if its driver isn't installed.
    pub fn open() -> io::Result<Self> {
        for path in hid_interfaces()? {
            // SAFETY: `path` is NUL-terminated; the handle is closed unless it's kept.
            unsafe {
                let handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                );
                // Windows keeps the touchpad and mouse collections to itself; those fail here.
                if handle == INVALID_HANDLE_VALUE {
                    continue;
                }
                if is_feed(handle) {
                    return Ok(Self { handle });
                }
                CloseHandle(handle);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the Crossglide virtual touchpad isn't installed; choose \"Install touchpad driver\" \
             in the tray menu",
        ))
    }

    /// Writes one feed report (see `report`); the touchpad reports it to Windows at once.
    pub fn send(&self, report: &[u8; FEED_REPORT_LEN]) -> io::Result<()> {
        let mut written = 0;
        // SAFETY: the buffer is valid for its length; the handle is open.
        let ok = unsafe {
            WriteFile(
                self.handle,
                report.as_ptr(),
                report.len() as u32,
                &mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Touchpad {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { CloseHandle(self.handle) };
    }
}

/// Device paths of every HID collection present, each NUL-terminated.
fn hid_interfaces() -> io::Result<Vec<Vec<u16>>> {
    // SAFETY: the GUID and list buffers are sized as the API asks; the list is read again if a
    // device arrives between the two calls.
    unsafe {
        let mut guid = std::mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        loop {
            let mut len = 0;
            let status = CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &guid,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            );
            if status != CR_SUCCESS {
                return Err(io::Error::other(format!(
                    "can't list HID devices (CONFIGRET {status})"
                )));
            }
            let mut list = vec![0u16; len as usize];
            let status = CM_Get_Device_Interface_ListW(
                &guid,
                ptr::null(),
                list.as_mut_ptr(),
                len,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            );
            if status == CR_SUCCESS {
                return Ok(list
                    .split(|&c| c == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| p.iter().copied().chain([0]).collect())
                    .collect());
            }
        }
    }
}

/// Whether `handle` is the virtual touchpad's feed collection.
///
/// # Safety
/// `handle` must be an open HID collection.
unsafe fn is_feed(handle: HANDLE) -> bool {
    // SAFETY: the caller's handle; the preparsed data is freed before returning.
    unsafe {
        let mut attributes: HIDD_ATTRIBUTES = std::mem::zeroed();
        attributes.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        if !HidD_GetAttributes(handle, &mut attributes)
            || attributes.VendorID != VENDOR_ID
            || attributes.ProductID != PRODUCT_ID
        {
            return false;
        }
        let mut preparsed = 0;
        if !HidD_GetPreparsedData(handle, &mut preparsed) {
            return false;
        }
        let mut caps: HIDP_CAPS = std::mem::zeroed();
        let ok = HidP_GetCaps(preparsed, &mut caps) == HIDP_STATUS_SUCCESS;
        HidD_FreePreparsedData(preparsed);
        ok && caps.UsagePage == FEED_USAGE_PAGE
            && caps.Usage == FEED_USAGE
            && caps.OutputReportByteLength as usize == FEED_REPORT_LEN
    }
}

/// Types keys by scan code, and remembers which are down so they can all be let go.
#[derive(Debug, Default)]
pub struct Keyboard {
    down: HashSet<u16>,
}

impl Keyboard {
    /// Presses or releases `scancode` (extended keys with [`EXTENDED`] set). Releasing a key
    /// this keyboard never pressed does nothing: its press went to the Mac, before control
    /// moved. Fails when Windows refuses the input, e.g. into an app running as administrator.
    pub fn key(&mut self, scancode: u16, down: bool) -> io::Result<()> {
        if down {
            self.down.insert(scancode);
        } else if !self.down.remove(&scancode) {
            return Ok(());
        }
        send_key(scancode, down)
    }

    /// Releases every key still down: when control leaves the PC or the Mac goes away.
    pub fn release_all(&mut self) {
        for scancode in self.down.drain() {
            let _ = send_key(scancode, false);
        }
    }
}

fn send_key(scancode: u16, down: bool) -> io::Result<()> {
    let mut flags = KEYEVENTF_SCANCODE;
    if scancode & 0xFF00 == EXTENDED {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scancode & 0xFF,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one well-formed INPUT.
    if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } != 1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes screen coordinates physical pixels on every monitor, the same ones the touchpad moves
/// the pointer in. Call once, before anything reads or moves the pointer.
pub fn per_monitor_dpi() {
    // SAFETY: a plain call; it fails harmlessly if the process already chose its awareness.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// The monitors, in virtual-screen coordinates.
pub fn displays() -> Vec<Rect> {
    unsafe extern "system" fn add(_: HMONITOR, _: HDC, rect: *mut RECT, list: LPARAM) -> i32 {
        // SAFETY: `list` is the Vec below and `rect` the monitor's rectangle, both valid for
        // the duration of EnumDisplayMonitors.
        unsafe {
            let r = &*rect;
            (*(list as *mut Vec<Rect>)).push(Rect {
                x: f64::from(r.left),
                y: f64::from(r.top),
                w: f64::from(r.right - r.left),
                h: f64::from(r.bottom - r.top),
            });
        }
        1
    }
    let mut list: Vec<Rect> = Vec::new();
    // SAFETY: the callback only runs during this call, while `list` lives.
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(add),
            &mut list as *mut Vec<Rect> as *mut c_void as LPARAM,
        )
    };
    list
}

/// Where the pointer is, or `None` if Windows won't say (on a secure desktop, say).
pub fn pointer() -> Option<(f64, f64)> {
    let mut at = POINT { x: 0, y: 0 };
    // SAFETY: a plain call into a local.
    (unsafe { GetCursorPos(&mut at) } != 0).then(|| (f64::from(at.x), f64::from(at.y)))
}

/// Whether the pointer is on `edge` of the desktop, and how far along; see [`edge::at_edge`].
pub fn pointer_at_edge(displays: &[Rect], edge: Edge) -> Option<f64> {
    let (x, y) = pointer()?;
    edge::at_edge(displays, x, y, edge)
}

/// Puts the pointer just inside `edge`, `along` the way, as control arrives from the Mac.
pub fn place_pointer(edge: Edge, along: f64) {
    if let Some((x, y)) = edge::entry_point(&displays(), edge, along) {
        // SAFETY: a plain call.
        unsafe { SetCursorPos(x.round() as i32, y.round() as i32) };
    }
}
