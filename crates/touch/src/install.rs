//! Installing the virtual touchpad's device (Windows, run as an administrator). The driver
//! package itself goes in with `pnputil /add-driver … /install`, but `pnputil` can't create the
//! root-enumerated device the driver binds to (it has no `/add-device` on Windows 11 build
//! 26200), so that's done here with SetupAPI.

use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::ptr;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIGCF_PRESENT, SP_DEVINFO_DATA, SPDRP_HARDWAREID,
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW,
};
use windows_sys::core::GUID;

/// The hardware ID the driver's `.inf` matches.
pub const HARDWARE_ID: &str = r"Root\CrossglideTouchpad";

/// The HID device class, `{745A17A0-74D3-11D0-B6FE-00A0C90F57DA}`.
const HID_CLASS: GUID = GUID {
    data1: 0x745a_17a0,
    data2: 0x74d3,
    data3: 0x11d0,
    data4: [0xb6, 0xfe, 0x00, 0xa0, 0xc9, 0x0f, 0x57, 0xda],
};

/// Whether `buf`, a `REG_MULTI_SZ` (strings separated by NULs, ending in two), lists `id`.
fn multi_sz_has(buf: &[u16], id: &str) -> bool {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .any(|s| String::from_utf16_lossy(s).eq_ignore_ascii_case(id))
}

fn failed(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Owns a device information set and destroys it.
struct DeviceSet(isize);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupAPI and is destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0 as _) };
    }
}

fn info() -> SP_DEVINFO_DATA {
    // SAFETY: an all-zero SP_DEVINFO_DATA is valid once `cbSize` is set.
    let mut info: SP_DEVINFO_DATA = unsafe { zeroed() };
    info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    info
}

/// Whether the virtual touchpad's device is present, with a driver or without.
pub fn device_exists() -> io::Result<bool> {
    // SAFETY: the buffers are as long as the sizes passed; the set is destroyed by the guard.
    unsafe {
        let list = SetupDiGetClassDevsW(&HID_CLASS, ptr::null(), ptr::null_mut(), DIGCF_PRESENT);
        if list as isize == -1 {
            return Err(failed("can't list the HID devices"));
        }
        let list = DeviceSet(list as isize);
        let mut index = 0;
        loop {
            let mut device = info();
            if SetupDiEnumDeviceInfo(list.0 as _, index, &mut device) == 0 {
                return Ok(false);
            }
            index += 1;
            let mut ids = [0u16; 512];
            let mut size = 0;
            let ok = SetupDiGetDeviceRegistryPropertyW(
                list.0 as _,
                &device,
                SPDRP_HARDWAREID,
                ptr::null_mut(),
                ids.as_mut_ptr().cast(),
                size_of_val(&ids) as u32,
                &mut size,
            );
            let used = (size as usize / 2).min(ids.len());
            if ok != 0 && multi_sz_has(&ids[..used], HARDWARE_ID) {
                return Ok(true);
            }
        }
    }
}

/// Creates the device the driver binds to, unless it's there already; returns whether it made
/// one. Needs an administrator. The driver package is installed separately (`pnputil`).
pub fn ensure_device() -> io::Result<bool> {
    if device_exists()? {
        return Ok(false);
    }
    // SAFETY: the strings are NUL-terminated and outlive the calls; the set is destroyed by the
    // guard.
    unsafe {
        let list = SetupDiCreateDeviceInfoList(&HID_CLASS, ptr::null_mut());
        if list as isize == -1 {
            return Err(failed("can't create a device set"));
        }
        let list = DeviceSet(list as isize);
        let mut device = info();
        let class: Vec<u16> = "HIDClass\0".encode_utf16().collect();
        if SetupDiCreateDeviceInfoW(
            list.0 as _,
            class.as_ptr(),
            &HID_CLASS,
            ptr::null(),
            ptr::null_mut(),
            DICD_GENERATE_ID,
            &mut device,
        ) == 0
        {
            return Err(failed("can't create the device"));
        }
        let id: Vec<u16> = HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
        if SetupDiSetDeviceRegistryPropertyW(
            list.0 as _,
            &mut device,
            SPDRP_HARDWAREID,
            id.as_ptr().cast(),
            size_of_val(id.as_slice()) as u32,
        ) == 0
        {
            return Err(failed("can't set the device's hardware ID"));
        }
        if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, list.0 as _, &device) == 0 {
            return Err(failed("can't register the device"));
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(ids: &[&str]) -> Vec<u16> {
        let mut out = Vec::new();
        for id in ids {
            out.extend(id.encode_utf16());
            out.push(0);
        }
        out.push(0);
        out
    }

    #[test]
    fn the_hardware_id_is_found_in_a_multi_string() {
        let buf = wide(&[r"HID\VID_1209&PID_C6D0", r"root\crossglidetouchpad"]);
        assert!(multi_sz_has(&buf, HARDWARE_ID));
        assert!(!multi_sz_has(&wide(&[r"ACPI\PNP0303"]), HARDWARE_ID));
        assert!(!multi_sz_has(&[], HARDWARE_ID));
        // Whatever follows the double NUL is stale buffer, not a second list.
        let mut buf = wide(&[r"ACPI\PNP0303"]);
        buf.extend(HARDWARE_ID.encode_utf16());
        assert!(!multi_sz_has(&buf, HARDWARE_ID));
    }
}
