//! Detect the WebView2 Evergreen Runtime.
//!
//! V1: detection only — the full WebView2-based UI is not implemented yet
//! and the installer always falls back to the modernized Win32 UI.
//! Detection is wired so the front-end can be swapped in cleanly.

#![cfg(windows)]

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_VALUE_TYPE, RegCloseKey,
    RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::PCWSTR;

const EDGE_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// Returns `Some(version)` if the WebView2 Evergreen Runtime is installed.
pub fn detect() -> Option<String> {
    // System-wide install
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for path in [
            format!(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{}", EDGE_GUID),
            format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{}", EDGE_GUID),
        ] {
            if let Some(v) = read_pv(root, &path) {
                if !v.is_empty() && v != "0.0.0.0" {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn read_pv(root: HKEY, path: &str) -> Option<String> {
    let path_w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(path_w.as_ptr()), None, KEY_READ, &mut hkey).is_err() {
            return None;
        }
        let name: Vec<u16> = "pv".encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = [0u16; 256];
        let mut sz: u32 = (buf.len() * 2) as u32;
        let mut ty = REG_VALUE_TYPE::default();
        let rc = RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut sz),
        );
        let _ = RegCloseKey(hkey);
        if rc.is_err() {
            return None;
        }
        let chars = (sz as usize / 2).min(buf.len());
        let end = buf[..chars].iter().position(|&c| c == 0).unwrap_or(chars);
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}
