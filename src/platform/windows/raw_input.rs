//! Raw Input: device enumeration and a capture thread.
//!
//! Raw Input reports which physical device produced each keystroke, without
//! elevation and without the window having focus (`RIDEV_INPUTSINK`). It
//! cannot block keystrokes. The capture thread can additionally install a
//! low-level keyboard hook, which can block keys but does not know their
//! source device; `rustclick-diag` uses both to measure how they interleave.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;
use std::{io, mem, ptr};

use windows_sys::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, HRAWINPUT, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RID_DEVICE_INFO, RID_INPUT,
    RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICEINFO, RIDI_DEVICENAME, RIM_TYPEHID,
    RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GIDC_ARRIVAL,
    GIDC_REMOVAL, GetMessageTime, GetMessageW, HC_ACTION, HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT,
    MSG, PostThreadMessageW, RegisterClassExW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WM_QUIT, WNDCLASSEXW,
};

use super::{from_wide, wide};

pub const USAGE_PAGE_GENERIC: u16 = 0x01;
pub const USAGE_MOUSE: u16 = 0x02;
pub const USAGE_KEYBOARD: u16 = 0x06;
pub const USAGE_PAGE_CONSUMER: u16 = 0x0C;
pub const USAGE_CONSUMER_CONTROL: u16 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    Mouse,
    Keyboard,
    Hid,
}

impl RawKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Keyboard => "keyboard",
            Self::Hid => "hid",
        }
    }
}

/// One Raw Input device (a HID top-level collection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDevice {
    /// The `hDevice` handle; valid while the device is connected.
    pub handle: usize,
    pub kind: RawKind,
    /// Device interface path, e.g. `\\?\HID#VID_046D&PID_C53E&MI_00#...`.
    pub name: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub usage_page: u16,
    pub usage: u16,
    /// Kind-specific details for logs.
    pub info: String,
}

/// Extracts vendor and product ids from a device interface path. Handles USB
/// (`VID_046D&PID_C53E`) and Bluetooth LE (`VID&02046d_PID&b503`) forms.
pub fn parse_vid_pid(name: &str) -> (Option<u16>, Option<u16>) {
    let upper = name.to_ascii_uppercase();
    let field = |key: &str| -> Option<u16> {
        let start = upper.find(key)? + key.len();
        let digits: String = upper[start..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        // Bluetooth paths prefix the id with a 2-digit source (e.g. "02").
        let digits = &digits[digits.len().saturating_sub(4)..];
        (digits.len() == 4).then(|| u16::from_str_radix(digits, 16).ok())?
    };
    (
        field("VID_").or_else(|| field("VID&")),
        field("PID_").or_else(|| field("PID&")),
    )
}

/// All Raw Input devices currently known to the system.
pub fn list_devices() -> Vec<RawDevice> {
    let size = mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0u32;
    // SAFETY: a null buffer queries the device count.
    if unsafe { GetRawInputDeviceList(ptr::null_mut(), &mut count, size) } == u32::MAX {
        return Vec::new();
    }
    // Devices can arrive between the two calls; leave some headroom.
    let mut list = vec![
        RAWINPUTDEVICELIST {
            hDevice: ptr::null_mut(),
            dwType: 0,
        };
        count as usize + 8
    ];
    let mut capacity = list.len() as u32;
    // SAFETY: `list` holds `capacity` entries.
    let n = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &mut capacity, size) };
    if n == u32::MAX {
        return Vec::new();
    }
    list.truncate(n as usize);
    list.iter()
        .filter_map(|d| device(d.hDevice as usize))
        .collect()
}

/// Details of one device, or `None` if it disappeared.
pub fn device(handle: usize) -> Option<RawDevice> {
    let h = handle as HANDLE;
    let mut len = 0u32;
    // SAFETY: a null buffer queries the name length in characters.
    unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, ptr::null_mut(), &mut len) };
    let mut buf = vec![0u16; len as usize + 1];
    let mut cap = buf.len() as u32;
    // SAFETY: `buf` holds `cap` UTF-16 units.
    let r =
        unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, buf.as_mut_ptr().cast(), &mut cap) };
    if r == u32::MAX {
        return None;
    }
    let name = from_wide(&buf);

    // SAFETY: RID_DEVICE_INFO is plain data; cbSize must be set before the call.
    let mut info: RID_DEVICE_INFO = unsafe { mem::zeroed() };
    info.cbSize = mem::size_of::<RID_DEVICE_INFO>() as u32;
    let mut size = info.cbSize;
    // SAFETY: `info` is a properly sized RID_DEVICE_INFO.
    let r =
        unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICEINFO, (&raw mut info).cast(), &mut size) };
    if r == u32::MAX {
        return None;
    }
    let (mut vid, mut pid) = parse_vid_pid(&name);
    // SAFETY: the union member matches `dwType`.
    let (kind, usage_page, usage, details) = unsafe {
        match info.dwType {
            RIM_TYPEKEYBOARD => {
                let k = info.Anonymous.keyboard;
                (
                    RawKind::Keyboard,
                    USAGE_PAGE_GENERIC,
                    USAGE_KEYBOARD,
                    format!(
                        "type {} subtype {} mode {} keys {} fn {}",
                        k.dwType,
                        k.dwSubType,
                        k.dwKeyboardMode,
                        k.dwNumberOfKeysTotal,
                        k.dwNumberOfFunctionKeys
                    ),
                )
            }
            RIM_TYPEMOUSE => {
                let m = info.Anonymous.mouse;
                (
                    RawKind::Mouse,
                    USAGE_PAGE_GENERIC,
                    USAGE_MOUSE,
                    format!("id {} buttons {}", m.dwId, m.dwNumberOfButtons),
                )
            }
            _ => {
                let d = info.Anonymous.hid;
                vid = vid.or(Some(d.dwVendorId as u16));
                pid = pid.or(Some(d.dwProductId as u16));
                (
                    RawKind::Hid,
                    d.usUsagePage,
                    d.usUsage,
                    format!("version 0x{:04x}", d.dwVersionNumber),
                )
            }
        }
    };
    Some(RawDevice {
        handle,
        kind,
        name,
        vid,
        pid,
        usage_page,
        usage,
        info: details,
    })
}

/// Something the capture thread observed.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// `WM_INPUT` from a keyboard collection.
    Key {
        device: usize,
        vkey: u16,
        make_code: u16,
        /// `RI_KEY_*` flags; bit 0 set means key up.
        flags: u16,
        message: u32,
        /// `GetMessageTime` of the `WM_INPUT` message (ms since boot).
        msg_time: u32,
        at: Instant,
    },
    /// `WM_INPUT` from a mouse collection with button or wheel activity.
    MouseButtons {
        device: usize,
        button_flags: u16,
        button_data: u16,
        at: Instant,
    },
    /// `WM_INPUT` from a mouse collection with movement only.
    MouseMove {
        device: usize,
        at: Instant,
    },
    /// `WM_INPUT` from a generic HID collection (e.g. consumer control).
    Hid {
        device: usize,
        report: Vec<u8>,
        at: Instant,
    },
    /// The low-level keyboard hook saw a key.
    Hook {
        vk: u32,
        scan: u32,
        /// `LLKHF_*` flags.
        flags: u32,
        /// Event time (ms since boot).
        time: u32,
        extra: usize,
        at: Instant,
    },
    DeviceArrived(usize),
    DeviceRemoved(usize),
}

impl CaptureEvent {
    pub fn is_key_up(flags: u16) -> bool {
        flags & 1 != 0
    }
}

/// Which inputs the capture thread registers for.
#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    pub keyboard: bool,
    pub consumer: bool,
    pub mouse: bool,
    /// Install a `WH_KEYBOARD_LL` hook as well.
    pub keyboard_hook: bool,
}

/// Called on the capture thread for every event. Returning `true` for a
/// [`CaptureEvent::Hook`] event blocks the keystroke; the value is ignored
/// for all other events. It must return quickly: Windows silently removes
/// low-level hooks that time out.
pub type Handler = Box<dyn FnMut(&CaptureEvent) -> bool + Send>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

fn dispatch(event: CaptureEvent) -> bool {
    HANDLER.with(|h| match h.try_borrow_mut() {
        Ok(mut h) => h.as_mut().is_some_and(|f| f(&event)),
        // Re-entrant call (should not happen); never block in that case.
        Err(_) => false,
    })
}

/// A thread with a message-only window that receives Raw Input. Stopped on
/// drop.
pub struct Capture {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(options: CaptureOptions, handler: Handler) -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let join = std::thread::Builder::new()
            .name("rustclick-raw-input".into())
            .spawn(move || capture_thread(options, handler, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(io::Error::other(e))
            }
            Err(_) => Err(io::Error::other("raw input thread exited during startup")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(join) = self.join.take() {
            // SAFETY: posting to a thread id is harmless if the thread is gone.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = join.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn last_error(what: &str) -> String {
    // SAFETY: reads the calling thread's last-error value.
    format!("{what} failed (error {})", unsafe { GetLastError() })
}

fn capture_thread(
    options: CaptureOptions,
    handler: Handler,
    ready: mpsc::Sender<Result<u32, String>>,
) {
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
    // SAFETY: plain Win32 calls on this thread; every handle created here is
    // released before the function returns.
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let class = wide("RustClickRawInput");
        let wc = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..mem::zeroed()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            let _ = ready.send(Err(last_error("RegisterClassExW")));
            return;
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(Err(last_error("CreateWindowExW")));
            return;
        }

        let mut wanted = Vec::new();
        if options.keyboard {
            wanted.push((USAGE_PAGE_GENERIC, USAGE_KEYBOARD));
        }
        if options.consumer {
            wanted.push((USAGE_PAGE_CONSUMER, USAGE_CONSUMER_CONTROL));
        }
        if options.mouse {
            wanted.push((USAGE_PAGE_GENERIC, USAGE_MOUSE));
        }
        let register = |flags: u32, target: HWND| -> bool {
            let devices: Vec<RAWINPUTDEVICE> = wanted
                .iter()
                .map(|&(page, usage)| RAWINPUTDEVICE {
                    usUsagePage: page,
                    usUsage: usage,
                    dwFlags: flags,
                    hwndTarget: target,
                })
                .collect();
            RegisterRawInputDevices(
                devices.as_ptr(),
                devices.len() as u32,
                mem::size_of::<RAWINPUTDEVICE>() as u32,
            ) != 0
        };
        if !wanted.is_empty() && !register(RIDEV_INPUTSINK | RIDEV_DEVNOTIFY, hwnd) {
            let _ = ready.send(Err(last_error("RegisterRawInputDevices")));
            DestroyWindow(hwnd);
            return;
        }

        let hook: HHOOK = if options.keyboard_hook {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), instance, 0);
            if hook.is_null() {
                let _ = ready.send(Err(last_error("SetWindowsHookExW")));
                if !wanted.is_empty() {
                    register(RIDEV_REMOVE, ptr::null_mut());
                }
                DestroyWindow(hwnd);
                return;
            }
            hook
        } else {
            ptr::null_mut()
        };

        let _ = ready.send(Ok(GetCurrentThreadId()));
        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }

        if !hook.is_null() {
            UnhookWindowsHookEx(hook);
        }
        if !wanted.is_empty() {
            register(RIDEV_REMOVE, ptr::null_mut());
        }
        DestroyWindow(hwnd);
    }
    HANDLER.with(|h| *h.borrow_mut() = None);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            // SAFETY: lparam is the HRAWINPUT of this WM_INPUT message.
            if let Some(event) = unsafe { read_input(lparam as HRAWINPUT) } {
                dispatch(event);
            }
            // DefWindowProc performs the required cleanup for WM_INPUT.
            // SAFETY: forwarding the original message.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_INPUT_DEVICE_CHANGE => {
            match wparam as u32 {
                GIDC_ARRIVAL => dispatch(CaptureEvent::DeviceArrived(lparam as usize)),
                GIDC_REMOVAL => dispatch(CaptureEvent::DeviceRemoved(lparam as usize)),
                _ => false,
            };
            0
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Reads the RAWINPUT block of a WM_INPUT message.
///
/// # Safety
/// `handle` must come from the current WM_INPUT message.
unsafe fn read_input(handle: HRAWINPUT) -> Option<CaptureEvent> {
    let at = Instant::now();
    let header = mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    // SAFETY: a null buffer queries the size.
    unsafe { GetRawInputData(handle, RID_INPUT, ptr::null_mut(), &mut size, header) };
    if size == 0 {
        return None;
    }
    // u64 storage keeps the RAWINPUT pointer fields aligned.
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `size` bytes.
    let read = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            buf.as_mut_ptr().cast::<c_void>(),
            &mut size,
            header,
        )
    };
    if read == u32::MAX || (read as usize) < mem::size_of::<RAWINPUTHEADER>() {
        return None;
    }
    // SAFETY: the buffer holds a RAWINPUT written by the system, and the
    // union member read below matches `header.dwType`.
    unsafe {
        let raw = &*buf.as_ptr().cast::<RAWINPUT>();
        let device = raw.header.hDevice as usize;
        match raw.header.dwType {
            RIM_TYPEKEYBOARD => {
                let k = raw.data.keyboard;
                Some(CaptureEvent::Key {
                    device,
                    vkey: k.VKey,
                    make_code: k.MakeCode,
                    flags: k.Flags,
                    message: k.Message,
                    msg_time: GetMessageTime() as u32,
                    at,
                })
            }
            RIM_TYPEMOUSE => {
                let buttons = raw.data.mouse.Anonymous.ulButtons;
                let (button_flags, button_data) = (buttons as u16, (buttons >> 16) as u16);
                Some(if button_flags != 0 {
                    CaptureEvent::MouseButtons {
                        device,
                        button_flags,
                        button_data,
                        at,
                    }
                } else {
                    CaptureEvent::MouseMove { device, at }
                })
            }
            RIM_TYPEHID => {
                let hid = raw.data.hid;
                let len = (hid.dwSizeHid * hid.dwCount) as usize;
                let start = (&raw const raw.data.hid.bRawData).cast::<u8>();
                let available = read as usize - (start as usize - buf.as_ptr() as usize);
                let report = std::slice::from_raw_parts(start, len.min(available)).to_vec();
                Some(CaptureEvent::Hid { device, report, at })
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let block = dispatch(CaptureEvent::Hook {
            vk: k.vkCode,
            scan: k.scanCode,
            flags: k.flags,
            time: k.time,
            extra: k.dwExtraInfo,
            at: Instant::now(),
        });
        if block {
            return 1;
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_and_product_ids_are_parsed_from_paths() {
        assert_eq!(
            parse_vid_pid(r"\\?\HID#VID_046D&PID_C53E&MI_00#8&1b2c&0&0000#{884b96c3}"),
            (Some(0x046D), Some(0xC53E))
        );
        assert_eq!(
            parse_vid_pid(
                r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d_PID&b503_REV&0006#9&1"
            ),
            (Some(0x046D), Some(0xB503))
        );
        assert_eq!(parse_vid_pid(r"\\?\ACPI#PNP0303#4&1"), (None, None));
    }
}
