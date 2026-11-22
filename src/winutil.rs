use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, FindWindowW, MB_ICONERROR, MB_OK, MSGFLT_ALLOW, MessageBoxW,
    PostMessageW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
};

pub fn to_wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn copy_to_wide_buf(s: &str, buf: &mut [u16]) {
    if buf.is_empty() {
        return;
    }
    buf.fill(0);
    let max_units = buf.len() - 1;
    let mut written = 0usize;
    for ch in s.chars() {
        let mut tmp = [0u16; 2];
        let encoded = ch.encode_utf16(&mut tmp);
        if written + encoded.len() > max_units {
            break;
        }
        for &u in encoded.iter() {
            buf[written] = u;
            written += 1;
        }
    }
    buf[written] = 0;
}

pub struct SingleInstanceGuard {
    handle: HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

pub enum InstanceLock {
    Acquired(SingleInstanceGuard),
    AlreadyRunning,
}

pub fn acquire_single_instance(mutex_name: &str) -> Result<InstanceLock, String> {
    let wide = to_wide_null(mutex_name);
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, wide.as_ptr());
        let err = GetLastError();
        if handle.is_null() {
            // If an elevated instance already holds the mutex, a non-elevated caller gets ERROR_ACCESS_DENIED.
            if err == ERROR_ACCESS_DENIED || err == ERROR_ALREADY_EXISTS {
                return Ok(InstanceLock::AlreadyRunning);
            }
            return Err(format!("创建单实例互斥锁失败 (code={})", err));
        }
        if err == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return Ok(InstanceLock::AlreadyRunning);
        }
        Ok(InstanceLock::Acquired(SingleInstanceGuard { handle }))
    }
}

pub fn allow_uipi_message(hwnd: HWND, msg: u32) {
    unsafe {
        ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, ptr::null_mut());
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

pub fn relaunch_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法获取当前程序路径: {}", e))?;
    let verb = to_wide_null("runas");
    let file = to_wide_null(&exe.to_string_lossy());
    let cwd = exe
        .parent()
        .map(|p| to_wide_null(&p.to_string_lossy()))
        .unwrap_or_else(|| to_wide_null(""));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let params_str = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let params = to_wide_null(&params_str);

    let res = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            if params_str.is_empty() {
                ptr::null()
            } else {
                params.as_ptr()
            },
            cwd.as_ptr(),
            SW_SHOWNORMAL,
        )
    };

    if (res as isize) <= 32 {
        return Err(format!("UAC 提权请求被取消或失败 (code={})", res as isize));
    }
    Ok(())
}

pub fn open_file_in_shell(path: &Path) -> Result<(), String> {
    let verb = to_wide_null("open");
    let file = to_wide_null(&path.to_string_lossy());
    let res = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (res as isize) <= 32 {
        return Err(format!(
            "打开文件失败: {} (code={})",
            path.display(),
            res as isize
        ));
    }
    Ok(())
}

pub fn notify_tray_message(class_name: &str, msg: u32) -> bool {
    let class_w = to_wide_null(class_name);
    unsafe {
        let hwnd = FindWindowW(class_w.as_ptr(), ptr::null());
        if !hwnd.is_null() {
            PostMessageW(hwnd, msg, 0, 0);
            return true;
        }
    }
    false
}

pub fn focus_window_by_title(title: &str) -> bool {
    let title_w = to_wide_null(title);
    unsafe {
        let hwnd = FindWindowW(ptr::null(), title_w.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
            return true;
        }
    }
    false
}

pub fn show_error_box(title: &str, message: &str) {
    let title_w = to_wide_null(title);
    let msg_w = to_wide_null(message);
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn decode_system_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    unsafe {
        let len = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            ptr::null_mut(),
            0,
        );
        if len <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; len as usize];
        let written = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            len,
        );
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        String::from_utf16_lossy(&wide[..written as usize])
    }
}
