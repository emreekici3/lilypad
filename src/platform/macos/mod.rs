//! macOS backend: CoreGraphics to see which windows are on screen, the
//! Accessibility API to move, resize and focus them.

mod desktop;
mod events;
mod ffi;
mod hotkeys;
mod instant;
mod overlay;
mod tap;
mod vsync;
mod writer;

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_void};
use std::ptr;

use log::{debug, info, warn};

use self::ffi::*;
use self::overlay::OverlayWindow;
use super::{Backend, Border, Display, Modifier, MouseButton, WindowEvent, WindowInfo};
use crate::geometry::WinId;
use crate::geometry::{Dock, DockEdge, Rect, usable_area};

/// Owned CoreFoundation reference, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    /// Takes ownership of a +1 reference ("Create"/"Copy" functions).
    fn owned(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then(|| Self(ptr))
    }

    /// Retains a borrowed reference ("Get" functions).
    fn retained(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then(|| Self(unsafe { CFRetain(ptr) }))
    }

    fn ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn cfstring(s: &str) -> Cf {
    let ptr = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        )
    };
    Cf::owned(ptr).expect("CFStringCreateWithBytes failed")
}

fn string_value(v: CFTypeRef) -> Option<String> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = [0 as c_char; 512];
        if CFStringGetCString(
            v,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            kCFStringEncodingUTF8,
        ) == 0
        {
            return None;
        }
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}

fn i64_value(v: CFTypeRef) -> Option<i64> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        (CFNumberGetValue(v, kCFNumberSInt64Type, (&raw mut out).cast()) != 0).then_some(out)
    }
}

fn f64_value(v: CFTypeRef) -> Option<f64> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: f64 = 0.0;
        (CFNumberGetValue(v, kCFNumberFloat64Type, (&raw mut out).cast()) != 0).then_some(out)
    }
}

fn bool_value(v: CFTypeRef) -> Option<bool> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFBooleanGetTypeID() {
            return None;
        }
        Some(CFBooleanGetValue(v) != 0)
    }
}

/// Attribute and action names, created once.
struct Names {
    windows: Cf,
    position: Cf,
    size: Cf,
    subrole: Cf,
    standard_window: &'static str,
    fullscreen: Cf,
    focused_application: Cf,
    focused_window: Cf,
    main: Cf,
    frontmost: Cf,
    raise: Cf,
    enhanced_ui: Cf,
    destroyed: Cf,
    miniaturized: Cf,
    deminiaturized: Cf,
    hidden: Cf,
    app_hidden: Cf,
    app_shown: Cf,
    window_created: Cf,
    focused_window_changed: Cf,
    app_activated: Cf,
    window_moved: Cf,
    window_resized: Cf,
}

impl Names {
    fn new() -> Self {
        Self {
            windows: cfstring("AXWindows"),
            position: cfstring("AXPosition"),
            size: cfstring("AXSize"),
            subrole: cfstring("AXSubrole"),
            standard_window: "AXStandardWindow",
            fullscreen: cfstring("AXFullScreen"),
            focused_application: cfstring("AXFocusedApplication"),
            focused_window: cfstring("AXFocusedWindow"),
            main: cfstring("AXMain"),
            frontmost: cfstring("AXFrontmost"),
            raise: cfstring("AXRaise"),
            enhanced_ui: cfstring("AXEnhancedUserInterface"),
            destroyed: cfstring("AXUIElementDestroyed"),
            miniaturized: cfstring("AXWindowMiniaturized"),
            deminiaturized: cfstring("AXWindowDeminiaturized"),
            hidden: cfstring("AXHidden"),
            app_hidden: cfstring("AXApplicationHidden"),
            app_shown: cfstring("AXApplicationShown"),
            window_created: cfstring("AXWindowCreated"),
            focused_window_changed: cfstring("AXFocusedWindowChanged"),
            app_activated: cfstring("AXApplicationActivated"),
            window_moved: cfstring("AXWindowMoved"),
            window_resized: cfstring("AXWindowResized"),
        }
    }
}

fn copy_attr(element: AXUIElementRef, attr: &Cf) -> Option<Cf> {
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(element, attr.ptr(), &mut value) };
    if err == kAXErrorSuccess {
        Cf::owned(value)
    } else {
        None
    }
}

/// Where the app says a window is: its AX position and size attributes,
/// named `position` and `size`.
fn read_frame(element: AXUIElementRef, position: &Cf, size: &Cf) -> Option<Rect> {
    let mut point = CGPoint::default();
    let mut dims = CGSize::default();
    unsafe {
        let pos = copy_attr(element, position)?;
        let sz = copy_attr(element, size)?;
        if AXValueGetValue(pos.ptr(), kAXValueTypeCGPoint, (&raw mut point).cast()) == 0
            || AXValueGetValue(sz.ptr(), kAXValueTypeCGSize, (&raw mut dims).cast()) == 0
        {
            return None;
        }
    }
    Some(cg_to_rect(CGRect {
        origin: point,
        size: dims,
    }))
}

fn set_attr(element: AXUIElementRef, attr: &Cf, value: CFTypeRef) -> bool {
    unsafe { AXUIElementSetAttributeValue(element, attr.ptr(), value) == kAXErrorSuccess }
}

fn is_settable(element: AXUIElementRef, attr: &Cf) -> bool {
    let mut settable: Boolean = 0;
    let err = unsafe { AXUIElementIsAttributeSettable(element, attr.ptr(), &mut settable) };
    err == kAXErrorSuccess && settable != 0
}

fn window_id(element: AXUIElementRef) -> Option<WinId> {
    let mut id: CGWindowID = 0;
    let err = unsafe { _AXUIElementGetWindow(element, &mut id) };
    (err == kAXErrorSuccess && id != 0).then_some(id)
}

/// Returns whether lily may use the Accessibility API, asking the user for
/// permission (system dialog) when `prompt` is set.
/// Whether the *Switch to Desktop N* shortcut is on.
pub fn desktop_shortcut_on(n: usize) -> bool {
    desktop::shortcut(n).is_ok()
}

pub fn ensure_accessibility(prompt: bool) -> bool {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let value = if prompt {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        };
        let options = CFDictionaryCreate(
            ptr::null(),
            &raw const key,
            &raw const value,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        let options = Cf::owned(options);
        AXIsProcessTrustedWithOptions(options.as_ref().map_or(ptr::null(), Cf::ptr)) != 0
    }
}

/// One on-screen window as CoreGraphics reports it.
struct CgWindow {
    id: WinId,
    pid: i32,
    app: String,
    frame: Rect,
}

/// Below this opacity a window counts as gone: it is fading out as it
/// closes. Windows are opaque otherwise; a window's own see-through
/// background (a terminal's) does not change this.
const VISIBLE_ALPHA: f64 = 0.5;

/// Normal application windows, other than lily's own. `on_screen_only`
/// limits them to the active Space; otherwise windows on every Space,
/// minimised and hidden ones included, are listed.
fn cg_windows(on_screen_only: bool) -> Vec<CgWindow> {
    let mut out = Vec::new();
    let options = if on_screen_only {
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    } else {
        kCGWindowListOptionAll
    };
    let own_pid = std::process::id() as i64;
    unsafe {
        let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID);
        let Some(list) = Cf::owned(list) else {
            return out;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            // Layer 0 holds normal application windows; menus, the Dock,
            // overlays and the menu bar live on other layers.
            if i64_value(get(kCGWindowLayer)) != Some(0) {
                continue;
            }
            let (Some(id), Some(pid)) = (
                i64_value(get(kCGWindowNumber)),
                i64_value(get(kCGWindowOwnerPID)),
            ) else {
                continue;
            };
            if pid == own_pid {
                continue; // the drag highlight
            }
            // A closing window fades out, and stays listed until it is
            // fully transparent: about a third of a second, though it is
            // out of sight after the first tenth.
            if on_screen_only && f64_value(get(kCGWindowAlpha)).is_some_and(|a| a < VISIBLE_ALPHA) {
                continue;
            }
            let mut bounds = CGRect::default();
            let bounds_dict = get(kCGWindowBounds);
            if bounds_dict.is_null()
                || !CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut bounds)
            {
                continue;
            }
            out.push(CgWindow {
                id: id as WinId,
                pid: pid as i32,
                app: string_value(get(kCGWindowOwnerName)).unwrap_or_default(),
                frame: cg_to_rect(bounds),
            });
        }
    }
    out
}

/// Whether app `pid` shows a window above the normal ones, other than in
/// the menu bar and the Dock: Spotlight's search field, a launcher, a panel.
/// Spotlight counts whichever app has the keyboard: its panel need not
/// make it the focused app.
fn shows_panel(pid: Option<i32>) -> bool {
    unsafe {
        let Some(list) = Cf::owned(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )) else {
            return false;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            let owner = i64_value(get(kCGWindowOwnerPID));
            let ours = pid.is_some_and(|p| owner == Some(i64::from(p)))
                || string_value(get(kCGWindowOwnerName)).as_deref() == Some("Spotlight");
            if !ours || f64_value(get(kCGWindowAlpha)).is_some_and(|a| a <= 0.0) {
                continue;
            }
            // 20: the Dock; 24, 25: the menu bar and its status items, which
            // any app may own while it is idle.
            if let Some(layer) = i64_value(get(kCGWindowLayer))
                && layer > 0
                && !matches!(layer, 20 | 24 | 25)
            {
                return true;
            }
        }
    }
    false
}

/// Whether an open menu or popover contains a point.
fn covered_at(x: f32, y: f32) -> bool {
    let own_pid = std::process::id() as i64;
    unsafe {
        let Some(list) = Cf::owned(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )) else {
            return false;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            // Only the pop-up menu layers (menus, popovers): focusing a
            // window would close them. The Dock and the menu bar stay in
            // front of a raised window anyway, and other layers hold
            // overlays of all kinds.
            let covering = |l: i64| matches!(l, 100..=103);
            if !i64_value(get(kCGWindowLayer)).is_some_and(covering)
                || f64_value(get(kCGWindowAlpha)).is_some_and(|a| a <= 0.0)
                || i64_value(get(kCGWindowOwnerPID)) == Some(own_pid)
            {
                continue;
            }
            let mut bounds = CGRect::default();
            let bounds_dict = get(kCGWindowBounds);
            if !bounds_dict.is_null()
                && CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut bounds)
                && cg_to_rect(bounds).contains_point(x, y)
            {
                return true;
            }
        }
    }
    false
}

fn cg_to_rect(r: CGRect) -> Rect {
    Rect::new(
        r.origin.x.round() as i32,
        r.origin.y.round() as i32,
        r.size.width.round() as i32,
        r.size.height.round() as i32,
    )
}

/// A display as AppKit sees it, in CoreGraphics coordinates (top-left
/// origin).
struct Screen {
    id: CGDirectDisplayID,
    frame: Rect,
    /// The frame less the menu bar and Dock.
    visible: Rect,
}

/// Every display, the primary one (with the menu bar) first.
fn screens() -> Vec<Screen> {
    type MsgSendId = unsafe extern "C" fn(Id, Sel) -> Id;
    type MsgSendIdArg = unsafe extern "C" fn(Id, Sel, Id) -> Id;
    type MsgSendIndex = unsafe extern "C" fn(Id, Sel, usize) -> Id;
    type MsgSendUsize = unsafe extern "C" fn(Id, Sel) -> usize;
    type MsgSendU32 = unsafe extern "C" fn(Id, Sel) -> u32;

    unsafe fn rect_of(obj: Id, sel: Sel) -> CGRect {
        #[cfg(target_arch = "x86_64")]
        {
            type Stret = unsafe extern "C" fn(*mut CGRect, Id, Sel);
            let f: Stret =
                unsafe { std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn()) };
            let mut out = CGRect::default();
            unsafe { f(&mut out, obj, sel) };
            out
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            type Ret = unsafe extern "C" fn(Id, Sel) -> CGRect;
            let f: Ret = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
            unsafe { f(obj, sel) }
        }
    }

    let mut out = Vec::new();
    unsafe {
        let send_id: MsgSendId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_id_arg: MsgSendIdArg = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_index: MsgSendIndex = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_usize: MsgSendUsize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_u32: MsgSendU32 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let pool = objc_autoreleasePoolPush();
        let class = objc_getClass(c"NSScreen".as_ptr());
        let screens = if class.is_null() {
            ptr::null_mut()
        } else {
            send_id(class, sel_registerName(c"screens".as_ptr()))
        };
        let count = if screens.is_null() {
            0
        } else {
            send_usize(screens, sel_registerName(c"count".as_ptr()))
        };
        let number_key = cfstring("NSScreenNumber");
        // Cocoa puts the origin at the bottom-left of the primary display
        // (index 0, the one with the menu bar); CoreGraphics at its top-left.
        let mut primary_h = 0.0;
        for i in 0..count {
            let screen = send_index(screens, sel_registerName(c"objectAtIndex:".as_ptr()), i);
            if screen.is_null() {
                continue;
            }
            let frame = rect_of(screen, sel_registerName(c"frame".as_ptr()));
            let visible = rect_of(screen, sel_registerName(c"visibleFrame".as_ptr()));
            if i == 0 {
                primary_h = frame.size.height;
            }
            let flip = |r: CGRect| {
                Rect::new(
                    r.origin.x.round() as i32,
                    (primary_h - (r.origin.y + r.size.height)).round() as i32,
                    r.size.width.round() as i32,
                    r.size.height.round() as i32,
                )
            };
            let description = send_id(screen, sel_registerName(c"deviceDescription".as_ptr()));
            let number = if description.is_null() {
                ptr::null_mut()
            } else {
                send_id_arg(
                    description,
                    sel_registerName(c"objectForKey:".as_ptr()),
                    number_key.ptr().cast_mut(),
                )
            };
            let id = if number.is_null() {
                if i == 0 { CGMainDisplayID() } else { 0 }
            } else {
                send_u32(number, sel_registerName(c"unsignedIntValue".as_ptr()))
            };
            out.push(Screen {
                id,
                frame: flip(frame),
                visible: flip(visible),
            });
        }
        objc_autoreleasePoolPop(pool);
    }
    out.retain(|s| s.visible.w > 0 && s.visible.h > 0);
    out
}

/// The primary display's visible frame (menu bar and Dock excluded), in
/// CoreGraphics coordinates (top-left origin).
fn primary_screen_area() -> Option<Rect> {
    screens().into_iter().next().map(|s| s.visible)
}

/// What the Dock is doing, as far as a display is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DockState {
    /// Set to hide itself: windows may use the whole edge.
    Hidden,
    /// Shown, but on another display.
    Elsewhere,
    Shown(Dock),
    /// The Dock could not be asked; trust AppKit's visible frame.
    Unknown,
}

/// Reads the Dock's live state from the Dock itself. AppKit's visible frame
/// cannot be relied on for this: without an `NSApplication` run loop it is
/// only refreshed now and then, so it lags behind the Dock being shown,
/// hidden or moved.
struct DockApi {
    autohide: Option<CoreDockGetAutoHideEnabledFn>,
    orientation: Option<CoreDockGetOrientationAndPinningFn>,
    rect: Option<SLSGetDockRectWithReasonFn>,
    prefs_app: Cf,
    autohide_key: Cf,
}

/// Bringing an app forward with one of its windows made key. Asking an
/// app to activate through Accessibility is ignored unless the user just
/// pressed a key or clicked (macOS's "cooperative activation"), so focus
/// follows mouse would do nothing; this asks the window server directly.
struct FocusApi {
    psn: Option<GetProcessForPIDFn>,
    front: Option<SLPSSetFrontProcessWithOptionsFn>,
    post: Option<SLPSPostEventRecordToFn>,
}

impl FocusApi {
    fn new() -> Self {
        unsafe {
            Self {
                psn: lookup(c"GetProcessForPID"),
                front: lookup(c"_SLPSSetFrontProcessWithOptions"),
                post: lookup(c"SLPSPostEventRecordTo"),
            }
        }
    }

    /// Makes `pid` the front app and `wid` its key window. Returns whether
    /// it could.
    fn focus(&self, pid: i32, wid: WinId) -> bool {
        let (Some(get_psn), Some(front), Some(post)) = (self.psn, self.front, self.post) else {
            return false;
        };
        let mut psn = ProcessSerialNumber::default();
        unsafe {
            if get_psn(pid, &raw mut psn) != 0 {
                return false;
            }
            if front(&raw const psn, wid, CPS_USER_GENERATED) != 0 {
                return false;
            }
            // Two synthetic window-server events that make the window key.
            for kind in [1u8, 2u8] {
                let mut bytes = [0u8; 0xf8];
                bytes[0x04] = 0xf8;
                bytes[0x08] = kind;
                bytes[0x3a] = 0x10;
                bytes[0x20..0x30].fill(0xff);
                bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
                post(&raw const psn, bytes.as_ptr());
            }
        }
        true
    }
}

/// The main display's UUID, as the Space functions name displays.
fn main_display_uuid() -> Option<Cf> {
    display_uuid(unsafe { CGMainDisplayID() })
}

/// A display's UUID, as the Space functions name displays.
fn display_uuid(display: CGDirectDisplayID) -> Option<Cf> {
    unsafe {
        let uuid = Cf::owned(CGDisplayCreateUUIDFromDisplayID(display))?;
        Cf::owned(CFUUIDCreateString(ptr::null(), uuid.ptr()))
    }
}

type SLSGetWindowBoundsFn = unsafe extern "C" fn(i32, u32, *mut CGRect) -> i32;

/// Whether the main display's Spaces are still sliding. A swipe let go of
/// halfway takes its time going back, however it ended.
fn spaces_sliding(connection: i32) -> bool {
    type IsAnimating = unsafe extern "C" fn(i32, CFStringRef) -> bool;
    static CALL: std::sync::OnceLock<Option<IsAnimating>> = std::sync::OnceLock::new();
    let call = CALL.get_or_init(|| unsafe { lookup(c"SLSManagedDisplayIsAnimating") });
    match (call, main_display_uuid()) {
        (Some(call), Some(uuid)) => unsafe { call(connection, uuid.ptr()) },
        _ => false,
    }
}

/// Listing the Spaces and moving windows between them.
struct SpaceApi {
    list: Option<SLSCopyManagedDisplaySpacesFn>,
    compat: Option<SLSSpaceSetCompatIDFn>,
    workspace: Option<SLSSetWindowListWorkspaceFn>,
    managed_move: Option<SLSMoveWindowsToManagedSpaceFn>,
    spaces_key: Cf,
    id_key: Cf,
    type_key: Cf,
}

/// A tag no real Space uses, to address one Space while moving windows.
const MOVE_TAG: i32 = 0x6362_7279;

impl SpaceApi {
    fn new() -> Self {
        unsafe {
            Self {
                list: lookup(c"SLSCopyManagedDisplaySpaces"),
                compat: lookup(c"SLSSpaceSetCompatID"),
                workspace: lookup(c"SLSSetWindowListWorkspace"),
                managed_move: lookup(c"SLSMoveWindowsToManagedSpace"),
                spaces_key: cfstring("Spaces"),
                id_key: cfstring("ManagedSpaceID"),
                type_key: cfstring("type"),
            }
        }
    }

    /// The desktops (not full-screen apps) of every display, in the window
    /// server's order of the displays, each in Mission Control order.
    fn by_display(&self, cid: i32) -> Vec<Vec<u64>> {
        let Some(list) = self.list else {
            return Vec::new();
        };
        unsafe {
            let Some(displays) = Cf::owned(list(cid)) else {
                return Vec::new();
            };
            (0..CFArrayGetCount(displays.ptr()))
                .map(|i| self.desktops(CFArrayGetValueAtIndex(displays.ptr(), i)))
                .collect()
        }
    }

    /// The desktops of one display's entry in the window server's list.
    unsafe fn desktops(&self, dict: CFTypeRef) -> Vec<u64> {
        let mut out = Vec::new();
        unsafe {
            let spaces = CFDictionaryGetValue(dict, self.spaces_key.ptr());
            if spaces.is_null() || CFGetTypeID(spaces) != CFArrayGetTypeID() {
                return out;
            }
            for i in 0..CFArrayGetCount(spaces) {
                let space = CFArrayGetValueAtIndex(spaces, i);
                if space.is_null() || CFGetTypeID(space) != CFDictionaryGetTypeID() {
                    continue;
                }
                let kind = i64_value(CFDictionaryGetValue(space, self.type_key.ptr()));
                if kind.unwrap_or(0) != 0 {
                    continue; // a full-screen app
                }
                if let Some(id) = i64_value(CFDictionaryGetValue(space, self.id_key.ptr())) {
                    out.push(id as u64);
                }
            }
        }
        out
    }

    /// Asks the window server to put `wid` on Space `sid`, without
    /// disabling SIP: the Space is given a temporary tag and the window
    /// is sent to that tag.
    fn move_by_tag(&self, cid: i32, wid: WinId, sid: u64) {
        if let (Some(compat), Some(workspace)) = (self.compat, self.workspace) {
            unsafe {
                compat(cid, sid, MOVE_TAG);
                workspace(cid, &raw const wid, 1, MOVE_TAG);
                compat(cid, sid, 0);
            }
        }
    }

    /// The direct call, which older systems allow.
    fn move_direct(&self, cid: i32, sid: u64, number: Option<Cf>) {
        unsafe {
            if let (Some(managed_move), Some(number)) = (self.managed_move, number) {
                let value = number.ptr();
                if let Some(array) = Cf::owned(CFArrayCreate(
                    ptr::null(),
                    &raw const value,
                    1,
                    &raw const kCFTypeArrayCallBacks,
                )) {
                    managed_move(cid, array.ptr(), sid);
                }
            }
        }
    }
}

/// How many in a row can fail before the slower way is taken for good.
const INSTANT_MISSES: u8 = 3;
/// How long a move or switch at once is given to show.
const INSTANT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
const INSTANT_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// How often the border is looked at, and how far off it may be.
const BORDER_CHECK: std::time::Duration = std::time::Duration::from_millis(100);
const BORDER_SLACK: i32 = 2;
/// How long after a Space switch the border is drawn anew, for the slide to
/// have ended.
const BORDER_REDO: std::time::Duration = std::time::Duration::from_millis(350);

/// Looks up a function in the loaded frameworks. `T` must be the
/// function's pointer type.
unsafe fn lookup<T: Copy>(name: &CStr) -> Option<T> {
    debug_assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    let p = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if p.is_null() {
        warn!("{} is unavailable", name.to_string_lossy());
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&p) })
}

impl DockApi {
    fn new() -> Self {
        unsafe {
            Self {
                autohide: lookup(c"CoreDockGetAutoHideEnabled"),
                orientation: lookup(c"CoreDockGetOrientationAndPinning"),
                rect: lookup(c"SLSGetDockRectWithReason"),
                prefs_app: cfstring("com.apple.dock"),
                autohide_key: cfstring("autohide"),
            }
        }
    }

    fn hides(&self) -> bool {
        if let Some(f) = self.autohide {
            return unsafe { f() } != 0;
        }
        unsafe {
            CFPreferencesAppSynchronize(self.prefs_app.ptr());
            Cf::owned(CFPreferencesCopyAppValue(
                self.autohide_key.ptr(),
                self.prefs_app.ptr(),
            ))
            .and_then(|v| bool_value(v.ptr()))
            .unwrap_or(false)
        }
    }

    fn state(&self, connection: i32, display: Rect) -> DockState {
        if self.hides() {
            return DockState::Hidden;
        }
        let Some(get_rect) = self.rect else {
            return DockState::Unknown;
        };
        let mut cg = CGRect::default();
        let mut reason = 0;
        if unsafe { get_rect(connection, &mut cg, &mut reason) } != 0 {
            return DockState::Unknown;
        }
        let r = cg_to_rect(cg);
        if r.w <= 0 || r.h <= 0 {
            return DockState::Unknown;
        }
        let (cx, cy) = r.center();
        if !display.contains_point(cx, cy) {
            return DockState::Elsewhere;
        }
        let orientation = self.orientation.map(|f| {
            let (mut orientation, mut pinning) = (0, 0);
            unsafe { f(&mut orientation, &mut pinning) };
            orientation
        });
        let edge = match orientation {
            Some(DOCK_ORIENTATION_BOTTOM) => DockEdge::Bottom,
            Some(DOCK_ORIENTATION_LEFT) => DockEdge::Left,
            Some(DOCK_ORIENTATION_RIGHT) => DockEdge::Right,
            _ if r.w >= r.h => DockEdge::Bottom,
            _ if cx < display.center().0 => DockEdge::Left,
            _ => DockEdge::Right,
        };
        let thickness = match edge {
            DockEdge::Bottom => r.h,
            DockEdge::Left | DockEdge::Right => r.w,
        };
        DockState::Shown(Dock { edge, thickness })
    }
}

thread_local! {
    /// Filled by `ax_callback` while the run loop is pumped (main thread only).
    static EVENTS: std::cell::RefCell<Vec<WindowEvent>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `refcon` tags: a window's destruction carries its id; a move or resize
/// carries `MOVED_TAG` and the id; anything else (a window opened, focus,
/// minimise, restore, hide, show) only asks for a fresh look.
const CHANGED_TAG: usize = 1 << 40;
const MOVED_TAG: usize = 1 << 41;

unsafe extern "C" fn ax_callback(
    _observer: AXObserverRef,
    _element: AXUIElementRef,
    _notification: CFStringRef,
    refcon: *mut c_void,
) {
    let tag = refcon as usize;
    let event = if tag & CHANGED_TAG != 0 {
        WindowEvent::Changed
    } else if tag & MOVED_TAG != 0 {
        WindowEvent::Moved((tag & !MOVED_TAG) as WinId)
    } else {
        WindowEvent::Destroyed(tag as WinId)
    };
    EVENTS.with(|e| e.borrow_mut().push(event));
}

/// An application's AX element and the observer delivering its window events.
struct AppHandle {
    element: Cf,
    observer: Option<Cf>,
    /// Every app-wide notification is registered. An app still starting up
    /// may refuse them; they are tried again for a while.
    watching: bool,
    tries: u8,
}

impl Drop for AppHandle {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            unsafe {
                let source = AXObserverGetRunLoopSource(observer.ptr());
                if !source.is_null() {
                    CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                }
            }
        }
    }
}

/// Registers a notification. Returns whether it is (now or already) on.
fn observe(observer: &Cf, element: AXUIElementRef, notification: &Cf, refcon: usize) -> bool {
    let err = unsafe {
        AXObserverAddNotification(
            observer.ptr(),
            element,
            notification.ptr(),
            refcon as *mut c_void,
        )
    };
    err == kAXErrorSuccess || err == kAXErrorNotificationAlreadyRegistered
}

/// The app-wide notifications: a window opened, the focus moved, the app
/// came to the front, was hidden or shown. Returns whether all are on.
fn watch_app(observer: &Cf, app: AXUIElementRef, names: &Names) -> bool {
    // Every one is tried, even after one fails.
    let mut all = true;
    for name in [
        &names.window_created,
        &names.focused_window_changed,
        &names.app_activated,
        &names.app_hidden,
        &names.app_shown,
    ] {
        all &= observe(observer, app, name, CHANGED_TAG);
    }
    all
}

/// How many times to try watching an app that refuses (one still starting
/// up), a tenth of a second apart.
const MAX_WATCH_TRIES: u8 = 30;

/// Whether the process is still running.
fn process_alive(pid: i32) -> bool {
    unsafe { kill(pid, 0) == 0 }
}

/// One Accessibility write of a window's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Position,
    Size,
}

struct TrackedWindow {
    element: Cf,
    pid: i32,
    resizable: bool,
    movable: bool,
}

/// How many polls to keep looking for the AX element of a new CG window
/// before treating it as unmanageable.
const MAX_LOOKUPS: u8 = 20;

pub struct MacBackend {
    names: Names,
    system: Cf,
    apps: HashMap<i32, AppHandle>,
    windows: HashMap<WinId, TrackedWindow>,
    lookups: HashMap<WinId, u8>,
    display: Rect,
    /// The frames of the displays, the main one first.
    frames: Vec<Rect>,
    /// Windows that exist on any Space, as of the last `windows()` call.
    alive: HashSet<WinId>,
    /// The app of each of them.
    alive_apps: HashMap<WinId, String>,
    /// And the process of each, for one never seen on screen (hidden, or on
    /// another Space).
    alive_pids: HashMap<WinId, i32>,
    connection: i32,
    /// Wakes the animation up with the display.
    vsync: vsync::Vsync,
    /// Write the frames, a thread per app.
    writers: writer::Writers,
    /// The main display's highest refresh rate, frames per second.
    frame_rate: Option<f64>,
    dock: DockApi,
    focuser: FocusApi,
    /// The last usable area reported, to log changes.
    area: Rect,
    hotkeys: hotkeys::HotkeyManager,
    window_bounds: Option<SLSGetWindowBoundsFn>,
    space_api: SpaceApi,
    /// Moves and switches at once that failed in a row, and whether the
    /// window server may still heed the other ways of moving a window.
    move_misses: u8,
    switch_misses: u8,
    tag_moves: bool,
    direct_moves: bool,
    /// The ring around the focused window, once one was asked for.
    border: Option<OverlayWindow>,
    /// SketchyBar triggers still running, reaped as they end.
    triggers: Vec<std::process::Child>,
    /// The way of moving a window, and of switching Spaces, that last
    /// worked.
    last_move: Option<&'static str>,
    last_switch: Option<&'static str>,
    /// When the border was last checked, and whether the Spaces slid then.
    border_checked: std::time::Instant,
    border_sliding: bool,
    /// The border last asked for, and whether a slide may have left its
    /// window astray since it was shown.
    border_wanted: Option<Border>,
    border_stale: bool,
    /// The Space the border was last checked on, and when to draw it anew
    /// after a switch, once the switch is over.
    border_space: u64,
    border_redo: Option<std::time::Instant>,
}

impl Default for MacBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MacBackend {
    pub fn new() -> Self {
        let system =
            Cf::owned(unsafe { AXUIElementCreateSystemWide() }).expect("system-wide AX element");
        events::start();
        unsafe { AXUIElementSetMessagingTimeout(system.ptr(), 0.25) };
        Self {
            names: Names::new(),
            system,
            apps: HashMap::new(),
            windows: HashMap::new(),
            lookups: HashMap::new(),
            display: Rect::default(),
            frames: Vec::new(),
            alive: HashSet::new(),
            alive_apps: HashMap::new(),
            alive_pids: HashMap::new(),
            connection: unsafe { CGSMainConnectionID() },
            vsync: vsync::Vsync::default(),
            writers: writer::Writers::default(),
            frame_rate: None,
            dock: DockApi::new(),
            focuser: FocusApi::new(),
            area: Rect::default(),
            hotkeys: hotkeys::HotkeyManager::default(),
            window_bounds: unsafe { lookup(c"SLSGetWindowBounds") },
            space_api: SpaceApi::new(),
            move_misses: 0,
            switch_misses: 0,
            tag_moves: true,
            direct_moves: true,
            border: None,
            triggers: Vec::new(),
            last_move: None,
            last_switch: None,
            border_checked: std::time::Instant::now(),
            border_sliding: false,
            border_wanted: None,
            border_stale: false,
            border_space: 0,
            border_redo: None,
        }
    }

    fn app(&mut self, pid: i32) -> Option<AXUIElementRef> {
        if !self.apps.contains_key(&pid) {
            let app = Cf::owned(unsafe { AXUIElementCreateApplication(pid) })?;
            unsafe { AXUIElementSetMessagingTimeout(app.ptr(), 0.25) };
            // With "enhanced user interface" on (set by some accessibility
            // tools), apps animate every frame change themselves, which
            // fights our animation and makes moves sluggish.
            if copy_attr(app.ptr(), &self.names.enhanced_ui).and_then(|v| bool_value(v.ptr()))
                == Some(true)
            {
                set_attr(app.ptr(), &self.names.enhanced_ui, unsafe {
                    kCFBooleanFalse
                });
            }
            let observer = Self::create_observer(pid);
            let watching = observer
                .as_ref()
                .is_some_and(|obs| watch_app(obs, app.ptr(), &self.names));
            if !watching {
                debug!("pid {pid} is not ready to be watched yet");
            }
            self.apps.insert(
                pid,
                AppHandle {
                    element: app,
                    observer,
                    watching,
                    tries: 1,
                },
            );
        }
        self.apps.get(&pid).map(|a| a.element.ptr())
    }

    /// Watches every regular app that is running, so a window one of them
    /// opens later is reported even if it has none now.
    fn watch_running_apps(&mut self) {
        let me = std::process::id() as i32;
        for pid in events::running_app_pids() {
            if pid != me {
                self.app(pid);
            }
        }
    }

    /// Tries again to watch apps that refused while starting up.
    fn retry_watching(&mut self) {
        for (pid, handle) in &mut self.apps {
            if handle.watching || handle.tries >= MAX_WATCH_TRIES {
                continue;
            }
            handle.tries += 1;
            if handle.observer.is_none() {
                handle.observer = Self::create_observer(*pid);
            }
            handle.watching = handle
                .observer
                .as_ref()
                .is_some_and(|obs| watch_app(obs, handle.element.ptr(), &self.names));
            if handle.watching {
                debug!("watching pid {pid} after {} tries", handle.tries);
            } else if handle.tries == MAX_WATCH_TRIES {
                warn!(
                    "pid {pid} cannot be watched: its new windows are seen only when something else changes"
                );
            }
        }
    }

    /// An observer for `pid`, attached to this thread's run loop.
    fn create_observer(pid: i32) -> Option<Cf> {
        let mut raw: AXObserverRef = ptr::null();
        unsafe {
            if AXObserverCreate(pid, ax_callback, &mut raw) != kAXErrorSuccess {
                debug!("no AX observer for pid {pid}");
                return None;
            }
            let observer = Cf::owned(raw)?;
            let source = AXObserverGetRunLoopSource(observer.ptr());
            if source.is_null() {
                return None;
            }
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            Some(observer)
        }
    }

    /// Looks up the AX elements of an app's windows and tracks those in `wanted`.
    fn discover(&mut self, pid: i32, wanted: &HashSet<WinId>) {
        let Some(app) = self.app(pid) else { return };
        let Some(list) = copy_attr(app, &self.names.windows) else {
            return;
        };
        unsafe {
            if CFGetTypeID(list.ptr()) != CFArrayGetTypeID() {
                return;
            }
            for i in 0..CFArrayGetCount(list.ptr()) {
                let element = CFArrayGetValueAtIndex(list.ptr(), i);
                let Some(id) = window_id(element) else {
                    continue;
                };
                if !wanted.contains(&id) || self.windows.contains_key(&id) {
                    continue;
                }
                let Some(element) = Cf::retained(element) else {
                    continue;
                };
                let (resizable, movable) = self.classify(element.ptr());
                debug!(
                    "tracking window {id} (pid {pid}, resizable {resizable}, movable {movable})"
                );
                if let Some(obs) = self.apps.get(&pid).and_then(|a| a.observer.as_ref()) {
                    observe(obs, element.ptr(), &self.names.destroyed, id as usize);
                    observe(obs, element.ptr(), &self.names.miniaturized, CHANGED_TAG);
                    observe(obs, element.ptr(), &self.names.deminiaturized, CHANGED_TAG);
                    let moved = MOVED_TAG | id as usize;
                    observe(obs, element.ptr(), &self.names.window_moved, moved);
                    observe(obs, element.ptr(), &self.names.window_resized, moved);
                }
                self.windows.insert(
                    id,
                    TrackedWindow {
                        element,
                        pid,
                        resizable,
                        movable,
                    },
                );
            }
        }
    }

    /// Whether a window can be resized, and whether it
    /// can at least be moved. Only standard, non-fullscreen windows count.
    fn classify(&self, element: AXUIElementRef) -> (bool, bool) {
        let subrole = copy_attr(element, &self.names.subrole).and_then(|v| string_value(v.ptr()));
        if subrole.as_deref() != Some(self.names.standard_window) {
            return (false, false);
        }
        if copy_attr(element, &self.names.fullscreen).and_then(|v| bool_value(v.ptr()))
            == Some(true)
        {
            return (false, false);
        }
        let movable = is_settable(element, &self.names.position);
        (movable && is_settable(element, &self.names.size), movable)
    }
}

impl MacBackend {
    /// Has `frame` written to window `id` in the order of `steps`, by the
    /// window's app's writer thread. `last`: the window stays there (the
    /// end of an animation, or a frame set at once).
    fn write_frame(&mut self, id: WinId, frame: Rect, steps: &[Step], last: bool) {
        if steps.is_empty() {
            return;
        }
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let Some(element) = Cf::retained(win.element.ptr()) else {
            return;
        };
        let pid = win.pid;
        // `lily config log_level trace` traces every write: where the
        // window was asked to go, and where the app put it after the first
        // call and at the end. The reads wait for the app, so it slows
        // animations down.
        let trace = log::log_enabled!(target: "lilypad::frames", log::Level::Trace);
        self.writers.send(
            pid,
            writer::Job {
                id,
                element,
                frame,
                steps: steps.to_vec(),
                last,
                trace,
            },
        );
    }

    /// The writes that put a window at its final frame. As in yabai: macOS
    /// cuts a resize short where it would take the window past the screen
    /// edge, so when both change the size is set before the move (to fit
    /// where the window is going) and again after it (in case the old
    /// place cut it short).
    fn settle_steps(frame: Rect, prev: Rect) -> Vec<Step> {
        let moved = (frame.x, frame.y) != (prev.x, prev.y);
        let resized = (frame.w, frame.h) != (prev.w, prev.h);
        match (moved, resized) {
            (true, true) => vec![Step::Size, Step::Position, Step::Size],
            (true, false) => vec![Step::Position],
            (false, true) => vec![Step::Size],
            (false, false) => Vec::new(),
        }
    }
}

impl MacBackend {
    /// Waits for `done`, which a slow window server can leave for a moment.
    fn settles(&mut self, mut done: impl FnMut(&mut Self) -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < INSTANT_WAIT {
            if done(self) {
                return true;
            }
            std::thread::sleep(INSTANT_POLL);
        }
        done(self)
    }

    /// Moves a window to `space` without touching it.
    fn move_at_once(&mut self, id: WinId, space: u64) -> bool {
        let wid = i64::from(id);
        let number = Cf::owned(unsafe {
            CFNumberCreate(ptr::null(), kCFNumberSInt64Type, (&raw const wid).cast())
        });
        let Some(number) = number else {
            return false;
        };
        let value = number.ptr();
        let array = Cf::owned(unsafe {
            CFArrayCreate(
                ptr::null(),
                &raw const value,
                1,
                &raw const kCFTypeArrayCallBacks,
            )
        });
        let Some(array) = array else {
            return false;
        };
        instant::move_windows(array.ptr(), space)
            && self.settles(|b| b.window_spaces(&[id]).get(&id) == Some(&space))
    }

    /// Switches to `space` on the same display, without the slide. False
    /// leaves the switch to the keyboard shortcut.
    fn switch_at_once(&mut self, space: u64) -> bool {
        let current = self.active_space();
        if current == space {
            return true;
        }
        let spaces = self.display_spaces(current);
        let at = |wanted| spaces.iter().position(|s| *s == wanted);
        let (Some(from), Some(to)) = (at(current), at(space)) else {
            return false;
        };
        // Its Spaces do not follow swipes, and it is no fault of theirs.
        if spaces_sliding(self.connection) || self.mission_control_open() {
            return false;
        }
        let steps = to as i32 - from as i32;
        if instant::swipe(steps) && self.settles(|b| b.active_space() == space) {
            self.switch_misses = 0;
            return true;
        }
        self.switch_misses += 1;
        if self.switch_misses == INSTANT_MISSES {
            warn!("switching Spaces at once does not work here");
        }
        false
    }

    /// Whether Mission Control is open: the Dock then has a window as large
    /// as the display. It has none otherwise, whether it shows or not.
    fn mission_control_open(&self) -> bool {
        unsafe {
            let display = CGDisplayBounds(CGMainDisplayID());
            let options = kCGWindowListOptionOnScreenOnly;
            let Some(list) = Cf::owned(CGWindowListCopyWindowInfo(options, kCGNullWindowID)) else {
                return false;
            };
            (0..CFArrayGetCount(list.ptr())).any(|i| {
                let info = CFArrayGetValueAtIndex(list.ptr(), i);
                let owner = string_value(CFDictionaryGetValue(info, kCGWindowOwnerName));
                let bounds = CFDictionaryGetValue(info, kCGWindowBounds);
                let mut frame = CGRect::default();
                owner.as_deref() == Some("Dock")
                    && !bounds.is_null()
                    && CGRectMakeWithDictionaryRepresentation(bounds, &mut frame)
                    && frame.size.width >= display.size.width
                    && frame.size.height >= display.size.height
            })
        }
    }

    /// The number of the *Switch to Desktop N* shortcut for `space`. macOS
    /// numbers desktops on from one display to the next: with two on the
    /// first display, the second display's first is Desktop 3.
    fn desktop_number(&self, space: u64) -> Option<usize> {
        let n = self
            .space_api
            .by_display(self.connection)
            .into_iter()
            .flatten()
            .position(|s| s == space)
            .map(|i| i + 1);
        if n.is_none() {
            warn!("Space {space} is not among the desktops");
        }
        n
    }

    fn window_spaces(&mut self, ids: &[WinId]) -> HashMap<WinId, u64> {
        let mut out = HashMap::with_capacity(ids.len());
        for &id in ids {
            unsafe {
                let wid = i64::from(id);
                let Some(number) = Cf::owned(CFNumberCreate(
                    ptr::null(),
                    kCFNumberSInt64Type,
                    (&raw const wid).cast(),
                )) else {
                    continue;
                };
                let value = number.ptr();
                let Some(array) = Cf::owned(CFArrayCreate(
                    ptr::null(),
                    &raw const value,
                    1,
                    &raw const kCFTypeArrayCallBacks,
                )) else {
                    continue;
                };
                let Some(spaces) = Cf::owned(CGSCopySpacesForWindows(
                    self.connection,
                    kCGSAllSpacesMask,
                    array.ptr(),
                )) else {
                    continue;
                };
                // A window on several Spaces ("all desktops") counts as on the
                // first; minimised windows and hidden apps are on none.
                if CFArrayGetCount(spaces.ptr()) > 0
                    && let Some(space) = i64_value(CFArrayGetValueAtIndex(spaces.ptr(), 0))
                {
                    out.insert(id, space as u64);
                }
            }
        }
        out
    }

    /// A swipe can carry the border off with a Space and leave it there,
    /// where AppKit still believes it is in place. Puts it back when the
    /// window server has it elsewhere, and once the Spaces stop sliding.
    fn check_border(&mut self) {
        if self.border_wanted.is_none() || self.border_checked.elapsed() < BORDER_CHECK {
            return;
        }
        let now = std::time::Instant::now();
        self.border_checked = now;
        let space = self.active_space();
        if std::mem::replace(&mut self.border_space, space) != space {
            self.border_stale = true;
            self.border_redo = Some(now + BORDER_REDO);
        }
        // A swipe let go of goes back to the same Space, and can still
        // carry the border off.
        let sliding = spaces_sliding(self.connection);
        let settled = std::mem::replace(&mut self.border_sliding, sliding) && !sliding;
        if sliding {
            self.border_stale = true;
            self.border_redo = Some(now + BORDER_REDO);
        }
        let redo = !sliding && self.border_redo.is_some_and(|at| now >= at);
        if redo {
            self.border_redo = None;
        }
        let Some((want, number)) = self.border.as_ref().and_then(OverlayWindow::shown_at) else {
            return;
        };
        let off = |a: i32, b: i32| (a - b).abs() > BORDER_SLACK;
        let moved = !sliding
            && self.server_frame(number).is_some_and(|r| {
                off(r.x, want.x) || off(r.y, want.y) || off(r.w, want.w) || off(r.h, want.h)
            });
        if settled || moved || redo {
            debug!("drawing the border anew (moved: {moved})");
            self.border_stale = true;
            let wanted = self.border_wanted;
            self.show_border(wanted);
        }
    }

    fn active_space(&mut self) -> u64 {
        // The Space on the main display. The system-wide
        // "active" Space follows focus to other displays instead.
        unsafe {
            let space = main_display_uuid().map_or(0, |n| {
                CGSManagedDisplayGetCurrentSpace(self.connection, n.ptr())
            });
            if space != 0 {
                space
            } else {
                CGSGetActiveSpace(self.connection)
            }
        }
    }
}

impl Backend for MacBackend {
    fn hotkey_commands(&mut self) -> Vec<String> {
        self.hotkeys.poll();
        self.hotkeys.take_lines()
    }

    fn set_hotkeys(&mut self, text: &str) {
        self.hotkeys.set(text);
    }

    fn windows(&mut self) -> Vec<WindowInfo> {
        // Windows just sent to where they stay must be read there, not a
        // frame or two before (they would be sent back to where they were).
        self.writers
            .wait_for_last_frames(std::time::Duration::from_millis(100));
        let cg = cg_windows(true);
        let all = cg_windows(false);
        let present: HashSet<WinId> = cg.iter().map(|w| w.id).collect();
        self.alive = all
            .iter()
            .map(|w| w.id)
            .chain(present.iter().copied())
            .collect();
        // Keep the AX handles of windows on other Spaces: they stay valid,
        // and the app only lists the windows of the active Space.
        self.alive_apps = all
            .iter()
            .chain(cg.iter())
            .map(|w| (w.id, w.app.clone()))
            .collect();
        self.alive_pids = all.iter().chain(cg.iter()).map(|w| (w.id, w.pid)).collect();
        let alive = &self.alive;
        self.windows.retain(|id, _| alive.contains(id));
        self.lookups.retain(|id, _| present.contains(id));
        // An app is watched for as long as it runs, windows or not: with no
        // polling, a window it opens later is only seen through its observer.
        self.apps.retain(|pid, _| process_alive(*pid));
        self.writers.retain(process_alive);
        if events::take_rescan() {
            self.watch_running_apps();
        }
        self.retry_watching();

        // Ask each app with unknown windows for its AX windows, once per poll.
        let mut unknown: HashMap<i32, HashSet<WinId>> = HashMap::new();
        for w in &cg {
            if !self.windows.contains_key(&w.id)
                && self.lookups.get(&w.id).copied().unwrap_or(0) < MAX_LOOKUPS
            {
                unknown.entry(w.pid).or_default().insert(w.id);
            }
        }
        for (pid, ids) in unknown {
            if pid == std::process::id() as i32 {
                continue;
            }
            self.discover(pid, &ids);
            for id in ids.into_iter().filter(|id| !self.windows.contains_key(id)) {
                *self.lookups.entry(id).or_default() += 1;
            }
        }

        cg.into_iter()
            .map(|w| {
                let tracked = self.windows.get(&w.id);
                WindowInfo {
                    id: w.id,
                    frame: w.frame,
                    app: w.app,
                    resizable: tracked.is_some_and(|t| t.resizable),
                    movable: tracked.is_some_and(|t| t.movable),
                }
            })
            .collect()
    }

    fn focused_window(&mut self) -> Option<WinId> {
        let ax = copy_attr(self.system.ptr(), &self.names.focused_application)
            .and_then(|app| copy_attr(app.ptr(), &self.names.focused_window))
            .and_then(|window| window_id(window.ptr()));
        // The Accessibility API sometimes cannot answer (a busy app, a
        // timeout). The window in front is then the best guess: focusing a
        // window raises it, and CoreGraphics lists windows front to back.
        ax.or_else(|| cg_windows(true).first().map(|w| w.id))
    }

    fn tiling_area(&mut self) -> Rect {
        let full = cg_to_rect(unsafe { CGDisplayBounds(CGMainDisplayID()) });
        self.display = full;
        let rate = events::main_screen_frame_rate();
        if rate != self.frame_rate {
            info!("animating at {} frames a second", rate.unwrap_or(120.0));
            self.frame_rate = rate;
        }
        let visible = primary_screen_area().unwrap_or_else(|| {
            warn!("NSScreen unavailable; assuming a 25pt menu bar");
            Rect::new(full.x, full.y + 25, full.w, full.h - 25)
        });
        let dock = self.dock.state(self.connection, full);
        let area = match dock {
            DockState::Shown(d) => usable_area(full, visible, Some(d)),
            DockState::Hidden | DockState::Elsewhere => usable_area(full, visible, None),
            DockState::Unknown => visible,
        };
        if area != self.area {
            info!("usable area {area:?} (dock: {dock:?}, visible frame: {visible:?})");
            self.area = area;
        }
        area
    }

    fn displays(&mut self) -> Vec<Display> {
        let main = Display {
            area: self.tiling_area(),
            frame: self.display,
            space: self.active_space(),
        };
        let mut out = vec![main];
        for screen in screens() {
            if screen.frame == main.frame || screen.id == 0 {
                continue;
            }
            let space = display_uuid(screen.id).map_or(0, |uuid| unsafe {
                CGSManagedDisplayGetCurrentSpace(self.connection, uuid.ptr())
            });
            // With "Displays have separate Spaces" off, every display shows
            // the main display's Space; only the main one counts then.
            if space == 0 || out.iter().any(|d| d.space == space) {
                continue;
            }
            let area = match self.dock.state(self.connection, screen.frame) {
                DockState::Shown(d) => usable_area(screen.frame, screen.visible, Some(d)),
                DockState::Hidden | DockState::Elsewhere => {
                    usable_area(screen.frame, screen.visible, None)
                }
                DockState::Unknown => screen.visible,
            };
            out.push(Display {
                frame: screen.frame,
                area,
                space,
            });
        }
        let frames: Vec<Rect> = out.iter().map(|d| d.frame).collect();
        if frames != self.frames {
            info!("{} display(s): {out:?}", out.len());
            self.frames = frames;
        }
        out
    }

    fn set_frame(&mut self, id: WinId, frame: Rect) {
        self.write_frame(id, frame, &[Step::Size, Step::Position, Step::Size], true);
    }

    fn animate_frame(&mut self, id: WinId, frame: Rect, prev: Rect) {
        let moved = (frame.x, frame.y) != (prev.x, prev.y);
        // Place and size go every frame (apps take a few ms over a size; a
        // slower one skips frames, as its writer keeps only the latest).
        let resized = (frame.w, frame.h) != (prev.w, prev.h);
        let (cx, cy) = prev.center();
        let display = self
            .frames
            .iter()
            .copied()
            .find(|d| d.contains_point(cx, cy))
            .unwrap_or(self.display);
        let order = if crate::platform::position_first(frame, prev, display) {
            [Step::Position, Step::Size]
        } else {
            [Step::Size, Step::Position]
        };
        let steps: Vec<Step> = order
            .into_iter()
            .filter(|s| match s {
                Step::Position => moved,
                Step::Size => resized,
            })
            .collect();
        self.write_frame(id, frame, &steps, false);
    }

    fn update_frame(&mut self, id: WinId, frame: Rect, prev: Rect) {
        self.write_frame(id, frame, &Self::settle_steps(frame, prev), true);
    }

    fn window_apps(&mut self) -> HashMap<WinId, String> {
        self.alive_apps.clone()
    }

    fn alive_windows(&mut self) -> Option<HashSet<WinId>> {
        Some(self.alive.clone())
    }

    fn set_mouse_modifier(&mut self, modifier: Option<Modifier>) {
        tap::set_modifier(modifier);
    }

    fn mouse_gesture(&mut self) -> Option<MouseButton> {
        tap::gesture()
    }

    fn cursor(&mut self) -> Option<(f32, f32)> {
        if let Some(p) = tap::location() {
            return Some(p);
        }
        let event = Cf::owned(unsafe { CGEventCreate(ptr::null()) })?;
        let p = unsafe { CGEventGetLocation(event.ptr()) };
        Some((p.x as f32, p.y as f32))
    }

    fn pump(&mut self) {
        // Deliver pending AX notifications (they arrive as run loop sources).
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0) };
        self.triggers
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        // AppKit is only woken up once there is a ring to draw.
        if self.border.is_none() {
            return;
        }
        self.check_border();
        let app = unsafe {
            let class = objc_getClass(c"NSApplication".as_ptr());
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(class, sel_registerName(c"sharedApplication".as_ptr()))
        };
        if !app.is_null() {
            overlay::pump_app(app);
        }
    }

    fn window_frame(&mut self, id: WinId) -> Option<Rect> {
        let win = self.windows.get(&id)?;
        read_frame(win.element.ptr(), &self.names.position, &self.names.size)
    }

    fn server_frame(&mut self, id: WinId) -> Option<Rect> {
        let read = self.window_bounds?;
        let mut r = CGRect::default();
        (unsafe { read(self.connection, id, &raw mut r) } == 0).then(|| {
            Rect::new(
                r.origin.x.round() as i32,
                r.origin.y.round() as i32,
                r.size.width.round() as i32,
                r.size.height.round() as i32,
            )
        })
    }

    fn space_swiping(&mut self) -> bool {
        spaces_sliding(self.connection)
    }

    fn place_on_space(&mut self, id: WinId, space: u64) -> bool {
        // Trust only what the window server says afterwards. A call it
        // ignored once it will ignore again: no use waiting for it each time.
        if self.move_misses < INSTANT_MISSES {
            if self.move_at_once(id, space) {
                self.move_misses = 0;
                self.last_move = Some("at once");
                return true;
            }
            self.move_misses += 1;
            if self.move_misses == INSTANT_MISSES {
                warn!("moving windows between Spaces at once does not work here");
            }
        }
        if self.tag_moves {
            self.space_api.move_by_tag(self.connection, id, space);
            if self.window_spaces(&[id]).get(&id) == Some(&space) {
                self.last_move = Some("by tag");
                return true;
            }
            self.tag_moves = false;
            debug!("moving windows by tag does not work here");
        }
        if self.direct_moves {
            let wid = i64::from(id);
            let number = Cf::owned(unsafe {
                CFNumberCreate(ptr::null(), kCFNumberSInt64Type, (&raw const wid).cast())
            });
            self.space_api.move_direct(self.connection, space, number);
            if self.window_spaces(&[id]).get(&id) == Some(&space) {
                self.last_move = Some("directly");
                return true;
            }
            self.direct_moves = false;
            debug!("moving windows directly does not work here");
        }
        false
    }

    fn shown_space(&mut self) -> u64 {
        self.active_space()
    }

    fn display_spaces(&mut self, shown: u64) -> Vec<u64> {
        self.space_api
            .by_display(self.connection)
            .into_iter()
            .find(|spaces| spaces.contains(&shown))
            .unwrap_or_else(|| vec![shown])
    }

    fn show_space(&mut self, space: u64) -> Result<(), String> {
        let n = self.desktop_number(space).ok_or("no such Space")?;
        debug!("showing Space {space}: Desktop {n}");
        if self.switch_misses < INSTANT_MISSES && self.switch_at_once(space) {
            self.last_switch = Some("at once");
            return Ok(());
        }
        desktop::switch(n)?;
        self.last_switch = Some("with the Switch to Desktop shortcut");
        Ok(())
    }

    fn set_app_hidden(&mut self, id: WinId, hidden: bool) {
        // A window of a hidden app, or on another Space, is not tracked yet:
        // its process is known from the window list all the same.
        let pid = self
            .windows
            .get(&id)
            .map(|w| w.pid)
            .or_else(|| self.alive_pids.get(&id).copied());
        let Some(pid) = pid else {
            warn!("window {id} is gone: cannot hide or show its app");
            return;
        };
        let value = unsafe {
            if hidden {
                kCFBooleanTrue
            } else {
                kCFBooleanFalse
            }
        };
        let set = self
            .app(pid)
            .is_some_and(|app| set_attr(app, &self.names.hidden, value));
        if !set {
            warn!("macOS would not hide or show app {pid}");
        }
    }

    fn launch_app(&mut self, app: &str) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .args(["-a", app])
            .status()
            .map_err(|e| format!("cannot start {app}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("cannot start {app}"))
        }
    }

    fn warp_cursor(&mut self, x: f32, y: f32) {
        unsafe {
            CGWarpMouseCursorPosition(CGPoint {
                x: f64::from(x),
                y: f64::from(y),
            });
            // A warp freezes the pointer for a moment unless it is
            // reconnected to the mouse right away.
            CGAssociateMouseAndMouseCursorPosition(1);
        }
    }

    fn overview_open(&mut self) -> bool {
        self.mission_control_open()
    }

    fn panel_has_focus(&mut self) -> bool {
        let pid = copy_attr(self.system.ptr(), &self.names.focused_application).and_then(|app| {
            let mut pid = 0;
            (unsafe { AXUIElementGetPid(app.ptr(), &raw mut pid) } == 0).then_some(pid)
        });
        shows_panel(pid)
    }

    fn covered_at(&mut self, x: f32, y: f32) -> bool {
        covered_at(x, y)
    }

    fn trigger_sketchybar(&mut self, event: &str, vars: &[(&str, String)]) {
        let mut cmd = std::process::Command::new("sketchybar");
        cmd.args(["--trigger", event]);
        cmd.args(vars.iter().map(|(k, v)| format!("{k}={v}")));
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(child) => self.triggers.push(child),
            Err(e) => debug!("cannot run sketchybar: {e}"),
        }
    }

    fn space_methods(&mut self) -> String {
        let state = |misses: u8, works: bool| {
            if misses >= INSTANT_MISSES || !works {
                "does not work here"
            } else {
                "not ruled out"
            }
        };
        format!(
            "switch at once: {}\n\
             move at once: {}\n\
             move by tag: {}\n\
             move directly: {}\n\
             last switch: {}\n\
             last move: {}\n",
            state(self.switch_misses, true),
            state(self.move_misses, true),
            state(0, self.tag_moves),
            state(0, self.direct_moves),
            self.last_switch.unwrap_or("none yet"),
            self.last_move.unwrap_or("none yet"),
        )
    }

    fn show_border(&mut self, border: Option<Border>) {
        self.border_wanted = border;
        // A window a slide carried off can stay drawn where it went, though
        // the window server says it is in place: a new one is not.
        if border.is_some() && std::mem::take(&mut self.border_stale) {
            self.border = None;
        }
        let Some(b) = border else {
            if let Some(w) = &mut self.border {
                w.hide();
            }
            return;
        };
        if self.border.is_none() {
            self.border = OverlayWindow::ring();
            if self.border.is_none() {
                warn!("cannot create the border window");
            }
        }
        if let Some(w) = &mut self.border {
            w.show_ring(b.rect, b.color, b.width, b.radius);
        }
    }

    fn wait_frame(&mut self, remaining: std::time::Duration, refresh: std::time::Duration) {
        // A late refresh still beats a wrong guess, within reason.
        if self.vsync.wait(refresh + refresh / 2).is_err() {
            std::thread::sleep(remaining);
        }
    }

    fn stop_frames(&mut self) {
        self.vsync.stop();
    }

    fn frame_rate(&mut self) -> Option<f64> {
        self.frame_rate
    }

    fn wants_another_look(&mut self) -> bool {
        // A new window whose AX element has not turned up yet, or an app
        // that is still starting up: both settle within a second or two.
        // (A window found after a few tries keeps its count: skip those.)
        self.lookups
            .iter()
            .any(|(id, n)| *n < MAX_LOOKUPS && !self.windows.contains_key(id))
            || self
                .apps
                .values()
                .any(|a| !a.watching && a.tries < MAX_WATCH_TRIES)
    }

    fn take_events(&mut self) -> Vec<WindowEvent> {
        let mut events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        if events::take_changed() {
            events.push(WindowEvent::Changed);
        }
        for event in &events {
            if let WindowEvent::Destroyed(id) = event {
                // Its AX element is dead.
                self.windows.remove(id);
            }
        }
        events
    }

    fn mouse_down(&mut self) -> bool {
        unsafe {
            CGEventSourceButtonState(kCGEventSourceStateCombinedSessionState, kCGMouseButtonLeft)
        }
    }

    fn close(&mut self, id: WinId) -> bool {
        let Some(win) = self.windows.get(&id) else {
            return false;
        };
        let Some(button) = copy_attr(win.element.ptr(), &cfstring("AXCloseButton")) else {
            return false;
        };
        let press = cfstring("AXPress");
        unsafe { AXUIElementPerformAction(button.ptr(), press.ptr()) == kAXErrorSuccess }
    }

    fn focus(&mut self, id: WinId) {
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let (element, pid) = (win.element.ptr(), win.pid);
        let focused = self.focuser.focus(pid, id);
        unsafe {
            AXUIElementPerformAction(element, self.names.raise.ptr());
            set_attr(element, &self.names.main, kCFBooleanTrue);
        }
        if !focused && let Some(app) = self.app(pid) {
            set_attr(app, &self.names.frontmost, unsafe { kCFBooleanTrue });
        }
    }
}
