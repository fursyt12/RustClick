//! Top-level windows, their processes, and posting keystrokes to them.

use std::mem;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GUITHREADINFO, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetGUIThreadInfo, GetWindow,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsChild,
    IsIconic, IsWindow, IsWindowVisible, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    WM_KEYDOWN, WM_KEYUP, WS_EX_TOOLWINDOW,
};

use super::{from_wide, keys};

/// A top-level window as seen by `EnumWindows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopWindow {
    pub hwnd: usize,
    pub pid: u32,
    pub thread_id: u32,
    pub title: String,
    pub class: String,
    pub visible: bool,
    pub minimized: bool,
    /// Hidden by DWM (e.g. suspended UWP apps or windows on other desktops).
    pub cloaked: bool,
    pub tool_window: bool,
    pub owned: bool,
}

impl TopWindow {
    /// Whether this looks like a window a user would pick as a target.
    pub fn is_app_window(&self) -> bool {
        self.visible && !self.cloaked && !self.tool_window && !self.owned && !self.title.is_empty()
    }
}

pub fn window_text(hwnd: usize) -> String {
    let hwnd = hwnd as HWND;
    // SAFETY: plain queries on a window handle; buffers are sized by the API.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        from_wide(&buf[..n.max(0) as usize])
    }
}

pub fn window_class(hwnd: usize) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` holds 256 UTF-16 units.
    let n = unsafe { GetClassNameW(hwnd as HWND, buf.as_mut_ptr(), buf.len() as i32) };
    from_wide(&buf[..n.max(0) as usize])
}

/// Process id and GUI thread id of a window; `None` if it no longer exists.
pub fn window_owner(hwnd: usize) -> Option<(u32, u32)> {
    let mut pid = 0u32;
    // SAFETY: plain query on a window handle.
    let thread = unsafe { GetWindowThreadProcessId(hwnd as HWND, &mut pid) };
    (thread != 0).then_some((pid, thread))
}

/// The window the user is currently working in.
pub fn foreground_window() -> usize {
    // SAFETY: plain query.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() as usize }
}

pub fn is_window(hwnd: usize) -> bool {
    // SAFETY: plain query on a window handle.
    unsafe { IsWindow(hwnd as HWND) != 0 }
}

fn describe(hwnd: HWND) -> Option<TopWindow> {
    let (pid, thread_id) = window_owner(hwnd as usize)?;
    let mut cloaked = 0u32;
    // SAFETY: plain queries on a window handle; `cloaked` is a u32 as the
    // attribute requires.
    unsafe {
        let has_cloak = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&raw mut cloaked).cast(),
            mem::size_of::<u32>() as u32,
        ) == 0;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        Some(TopWindow {
            hwnd: hwnd as usize,
            pid,
            thread_id,
            title: window_text(hwnd as usize),
            class: window_class(hwnd as usize),
            visible: IsWindowVisible(hwnd) != 0,
            minimized: IsIconic(hwnd) != 0,
            cloaked: has_cloak && cloaked != 0,
            tool_window: ex_style & WS_EX_TOOLWINDOW != 0,
            owned: !GetWindow(hwnd, GW_OWNER).is_null(),
        })
    }
}

/// Every top-level window, in z-order.
pub fn top_level_windows() -> Vec<TopWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the `Vec<HWND>` passed below.
        unsafe { (*(lparam as *mut Vec<HWND>)).push(hwnd) };
        1
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    unsafe { EnumWindows(Some(collect), (&raw mut handles) as LPARAM) };
    handles.into_iter().filter_map(describe).collect()
}

/// What can be learned about a process without elevation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessDetails {
    pub executable: Option<String>,
    /// Creation time as a FILETIME value (100 ns since 1601). Together with
    /// the PID it identifies one process lifetime.
    pub created: Option<u64>,
    pub error: Option<String>,
}

pub fn process_details(pid: u32) -> ProcessDetails {
    // SAFETY: the handle is checked and closed; buffers are sized for the API.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return ProcessDetails {
                error: Some(format!(
                    "OpenProcess failed (error {})",
                    windows_sys::Win32::Foundation::GetLastError()
                )),
                ..ProcessDetails::default()
            };
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let executable =
            (QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
                != 0)
                .then(|| from_wide(&buf[..len as usize]));
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let created = (GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)
            != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime));
        CloseHandle(handle);
        ProcessDetails {
            executable,
            created,
            error: None,
        }
    }
}

/// All running processes as (pid, executable file name).
pub fn processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    // SAFETY: the snapshot handle is checked and closed; PROCESSENTRY32W is
    // plain data with dwSize set as required.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            out.push((entry.th32ProcessID, from_wide(&entry.szExeFile)));
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// The window that has keyboard focus within the given GUI thread, even if
/// that thread is not in the foreground.
pub fn thread_focus(thread_id: u32) -> Option<usize> {
    // SAFETY: GUITHREADINFO is plain data with cbSize set as required.
    unsafe {
        let mut info: GUITHREADINFO = mem::zeroed();
        info.cbSize = mem::size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(thread_id, &mut info) == 0 || info.hwndFocus.is_null() {
            return None;
        }
        Some(info.hwndFocus as usize)
    }
}

/// Whether `child` is `parent` or one of its descendants.
pub fn is_self_or_child(parent: usize, child: usize) -> bool {
    // SAFETY: plain query on window handles.
    parent == child || unsafe { IsChild(parent as HWND, child as HWND) != 0 }
}

fn key_lparam(vk: u16, up: bool) -> LPARAM {
    // SAFETY: pure lookup in the active keyboard layout.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } & 0xFF;
    let mut lparam = 1 | (scan << 16);
    if keys::is_extended(vk) {
        lparam |= 1 << 24;
    }
    if up {
        lparam |= (1 << 30) | (1 << 31);
    }
    lparam as LPARAM
}

/// Posts WM_KEYDOWN and WM_KEYUP for `vk` to the window's message queue.
/// Success means only that the messages were queued.
pub fn post_key(hwnd: usize, vk: u16) -> Result<(), String> {
    let hwnd = hwnd as HWND;
    // SAFETY: posting messages to a window handle; failures are reported.
    unsafe {
        if PostMessageW(hwnd, WM_KEYDOWN, vk as usize, key_lparam(vk, false)) == 0
            || PostMessageW(hwnd, WM_KEYUP, vk as usize, key_lparam(vk, true)) == 0
        {
            return Err(format!(
                "PostMessage failed (error {})",
                windows_sys::Win32::Foundation::GetLastError()
            ));
        }
    }
    Ok(())
}

/// Sends WM_KEYDOWN and WM_KEYUP synchronously, waiting up to `timeout_ms`
/// for each. Returns the window procedure's results.
pub fn send_key(hwnd: usize, vk: u16, timeout_ms: u32) -> Result<(usize, usize), String> {
    let hwnd = hwnd as HWND;
    let send = |msg, up| {
        let mut result = 0usize;
        // SAFETY: synchronous message with a timeout; `result` outlives the call.
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd,
                msg,
                vk as usize,
                key_lparam(vk, up),
                SMTO_ABORTIFHUNG,
                timeout_ms,
                &mut result,
            )
        };
        if ok == 0 {
            Err(format!(
                "SendMessageTimeout failed (error {})",
                // SAFETY: reads the calling thread's last-error value.
                unsafe { windows_sys::Win32::Foundation::GetLastError() }
            ))
        } else {
            Ok(result)
        }
    };
    Ok((send(WM_KEYDOWN, false)?, send(WM_KEYUP, true)?))
}

/// Executable file name from a full path (`C:\...\POWERPNT.EXE` → `POWERPNT.EXE`).
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Friendly application name for well-known presentation applications.
pub fn app_name(executable: &str) -> String {
    let file = file_name(executable);
    match file.to_ascii_lowercase().as_str() {
        "powerpnt.exe" => "Microsoft PowerPoint".into(),
        "acrobat.exe" => "Adobe Acrobat".into(),
        "acrord32.exe" => "Adobe Acrobat Reader".into(),
        _ => file
            .rsplit_once('.')
            .map_or(file, |(stem, _)| stem)
            .to_owned(),
    }
}

/// Window class of the PowerPoint slide show window.
pub const POWERPOINT_SLIDESHOW_CLASS: &str = "screenClass";

/// A visible PowerPoint slide show window of the given process, if any.
pub fn powerpoint_slideshow(pid: u32) -> Option<TopWindow> {
    top_level_windows()
        .into_iter()
        .find(|w| w.pid == pid && w.visible && w.class == POWERPOINT_SLIDESHOW_CLASS)
}
