//! Switching Spaces through the macOS "Switch to Desktop N" keyboard
//! shortcuts (System Settings > Keyboard > Keyboard Shortcuts > Mission
//! Control), for when doing it at once does not work. Only the keyboard: the
//! pointer is never touched.

use std::ptr;

use super::ffi::*;
use super::{Cf, bool_value, cfstring, i64_value};

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *const std::ffi::c_void,
        keycode: u16,
        down: bool,
    ) -> CFTypeRef;
    fn CGEventSetFlags(event: CFTypeRef, flags: u64);
    fn CGEventPost(tap: u32, event: CFTypeRef);
}

const HID_EVENT_TAP: u32 = 0;

/// Desktop n's shortcut has this id in `com.apple.symbolichotkeys`.
const FIRST_DESKTOP_HOTKEY: usize = 117;

/// The key code and modifier flags of the "Switch to Desktop n" shortcut.
pub fn shortcut(n: usize) -> Result<(u16, u64), String> {
    let missing = || {
        format!(
            "the shortcut \"Switch to Desktop {n}\" is off; turn it on in System Settings > \
             Keyboard > Keyboard Shortcuts > Mission Control"
        )
    };
    unsafe {
        let app = cfstring("com.apple.symbolichotkeys");
        let key = cfstring("AppleSymbolicHotKeys");
        CFPreferencesAppSynchronize(app.ptr());
        let hotkeys =
            Cf::owned(CFPreferencesCopyAppValue(key.ptr(), app.ptr())).ok_or_else(missing)?;
        if CFGetTypeID(hotkeys.ptr()) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let id = cfstring(&(FIRST_DESKTOP_HOTKEY + n).to_string());
        let entry = CFDictionaryGetValue(hotkeys.ptr(), id.ptr());
        if entry.is_null() || CFGetTypeID(entry) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let enabled = CFDictionaryGetValue(entry, cfstring("enabled").ptr());
        let on = bool_value(enabled).or_else(|| i64_value(enabled).map(|v| v != 0));
        if on != Some(true) {
            return Err(missing());
        }
        let value = CFDictionaryGetValue(entry, cfstring("value").ptr());
        if value.is_null() || CFGetTypeID(value) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let params = CFDictionaryGetValue(value, cfstring("parameters").ptr());
        if params.is_null()
            || CFGetTypeID(params) != CFArrayGetTypeID()
            || CFArrayGetCount(params) < 3
        {
            return Err(missing());
        }
        let keycode = i64_value(CFArrayGetValueAtIndex(params, 1)).ok_or_else(missing)?;
        let flags = i64_value(CFArrayGetValueAtIndex(params, 2)).ok_or_else(missing)?;
        Ok((keycode as u16, flags as u64))
    }
}

fn post_key(keycode: u16, flags: u64) {
    for down in [true, false] {
        unsafe {
            if let Some(e) = Cf::owned(CGEventCreateKeyboardEvent(ptr::null(), keycode, down)) {
                CGEventSetFlags(e.ptr(), flags);
                CGEventPost(HID_EVENT_TAP, e.ptr());
            }
        }
    }
}

/// Switches to Space n with its keyboard shortcut.
pub fn switch(n: usize) -> Result<(), String> {
    let (keycode, flags) = shortcut(n)?;
    post_key(keycode, flags);
    Ok(())
}
