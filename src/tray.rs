use std::path::PathBuf;
use std::ptr;
use std::thread;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW, KillTimer,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer,
    SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenuEx, TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
    WM_LBUTTONUP, WM_NCDESTROY, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};

use crate::autostart;
use crate::config::{Config, load_config};
use crate::icon::{OwnedIcon, TrayVisualState, create_state_icon};
use crate::net::{NetChangeWatcher, NetworkMode, NetworkStatus, SwitchTarget, query_status, switch_mode};
use crate::winutil::{copy_to_wide_buf, launch_gui_process, open_file_in_shell, to_wide_null};

pub const TRAY_WINDOW_CLASS: &str = "NetSwitcherTrayWindowClass";
const TRAY_ICON_ID: u32 = 1;

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_NET_CHANGED: u32 = WM_APP + 2;
const WM_APP_SWITCH_DONE: u32 = WM_APP + 3;
pub const WM_APP_RELOAD_CONFIG: u32 = WM_APP + 4;

const TIMER_POLL_ID: usize = 1;
const TIMER_DEBOUNCE_ID: usize = 2;

const CMD_TOGGLE: u32 = 1001;
const CMD_SET_STATIC: u32 = 1002;
const CMD_SET_DHCP: u32 = 1003;
const CMD_REFRESH: u32 = 1004;
const CMD_OPEN_GUI: u32 = 1005;
const CMD_OPEN_CONFIG: u32 = 1006;
const CMD_RELOAD_CONFIG: u32 = 1007;
const CMD_TOGGLE_AUTOSTART: u32 = 1008;
const CMD_EXIT: u32 = 1009;

struct SwitchResult {
    target: SwitchTarget,
    outcome: Result<NetworkStatus, String>,
}

struct AppState {
    hwnd: HWND,
    config_path: PathBuf,
    config: Config,
    status: NetworkStatus,
    switching: bool,
    current_visual: Option<TrayVisualState>,
    current_icon: Option<OwnedIcon>,
    taskbar_created_msg: u32,
    _watcher: Option<NetChangeWatcher>,
}

impl AppState {
    fn desired_visual(&self) -> TrayVisualState {
        if self.switching {
            return TrayVisualState::Switching;
        }
        match self.status.mode {
            NetworkMode::Static => TrayVisualState::Static,
            NetworkMode::StaticCustom => TrayVisualState::StaticCustom,
            NetworkMode::Dhcp => TrayVisualState::Dhcp,
            NetworkMode::Disconnected => TrayVisualState::Disconnected,
        }
    }

    fn ensure_icon(&mut self) -> Result<(), String> {
        let visual = self.desired_visual();
        if self.current_visual != Some(visual) || self.current_icon.is_none() {
            let icon = create_state_icon(visual)?;
            // Replacing self.current_icon automatically drops (DestroyIcon) the previous HICON
            self.current_icon = Some(icon);
            self.current_visual = Some(visual);
        }
        Ok(())
    }

    fn add_tray_icon(&mut self) -> Result<(), String> {
        self.ensure_icon()?;
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ICON_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        if let Some(ref icon) = self.current_icon {
            nid.hIcon = icon.handle();
        }
        let tip = self.status.tooltip_text(self.switching);
        copy_to_wide_buf(&tip, &mut nid.szTip);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
        if ok == 0 {
            return Err("添加系统托盘图标失败".to_string());
        }
        Ok(())
    }

    fn update_tray_icon(&mut self) {
        if self.ensure_icon().is_err() {
            return;
        }
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ICON_ID;
        nid.uFlags = NIF_ICON | NIF_TIP;
        if let Some(ref icon) = self.current_icon {
            nid.hIcon = icon.handle();
        }
        let tip = self.status.tooltip_text(self.switching);
        copy_to_wide_buf(&tip, &mut nid.szTip);

        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn show_notification(&self, title: &str, body: &str, is_error: bool) {
        if !self.config.ui.show_notifications && !is_error {
            return;
        }
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ICON_ID;
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if is_error { NIIF_ERROR } else { NIIF_INFO };
        copy_to_wide_buf(title, &mut nid.szInfoTitle);
        copy_to_wide_buf(body, &mut nid.szInfo);

        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn remove_tray_icon(&self) {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ICON_ID;
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    fn refresh_status(&mut self) {
        self.status = query_status(&self.config);
        self.update_tray_icon();
    }

    fn reset_poll_timer(&self) {
        let secs = self.config.ui.refresh_interval_seconds.max(3);
        unsafe {
            KillTimer(self.hwnd, TIMER_POLL_ID);
            SetTimer(self.hwnd, TIMER_POLL_ID, secs.saturating_mul(1000), None);
        }
    }

    fn start_switch(&mut self, target: SwitchTarget) {
        if self.switching {
            return;
        }
        self.switching = true;
        self.update_tray_icon();

        let cfg = self.config.clone();
        let hwnd_raw = self.hwnd as isize;
        thread::spawn(move || {
            let outcome = switch_mode(&cfg, target);
            let boxed = Box::new(SwitchResult { target, outcome });
            let ptr = Box::into_raw(boxed) as LPARAM;
            unsafe {
                PostMessageW(hwnd_raw as HWND, WM_APP_SWITCH_DONE, 0, ptr);
            }
        });
    }

    fn toggle_mode(&mut self) {
        if self.switching {
            return;
        }
        self.status = query_status(&self.config);
        let target = match self.status.mode {
            NetworkMode::Dhcp | NetworkMode::Disconnected | NetworkMode::StaticCustom => {
                SwitchTarget::Static
            }
            NetworkMode::Static => SwitchTarget::Dhcp,
        };
        self.start_switch(target);
    }

    fn handle_switch_done(&mut self, res: SwitchResult) {
        self.switching = false;
        match res.outcome {
            Ok(new_status) => {
                self.status = new_status;
                self.update_tray_icon();
                let title = format!("已切换到 {}", res.target.display_name());
                let body = format!(
                    "网卡: {}\n{}",
                    self.status.interface_name,
                    self.status.summary_line()
                );
                self.show_notification(&title, &body, false);
            }
            Err(err) => {
                self.status = query_status(&self.config);
                self.update_tray_icon();
                let title = format!("切换到 {} 失败", res.target.display_name());
                self.show_notification(&title, &err, true);
            }
        }
    }

    fn reload_config(&mut self) {
        match load_config(&self.config_path) {
            Ok(new_cfg) => {
                self.config = new_cfg;
                self.reset_poll_timer();
                self.refresh_status();
                self.show_notification(
                    "配置已重新加载",
                    &format!("目标网卡: {}", self.config.interface_name),
                    false,
                );
            }
            Err(e) => {
                self.show_notification("重新加载配置失败", &e, true);
            }
        }
    }

    fn toggle_autostart(&mut self) {
        let next = !autostart::is_enabled();
        match autostart::set_enabled(next) {
            Ok(()) => {
                let msg = if next {
                    "已开启登录时最高权限静默自启（免 UAC）"
                } else {
                    "已关闭开机自启动"
                };
                self.show_notification("开机自启动", msg, false);
            }
            Err(e) => {
                self.show_notification("设置开机自启动失败", &e, true);
            }
        }
    }

    fn show_context_menu(&mut self) {
        // Refresh status right before showing menu so summary is always up-to-date
        if !self.switching {
            self.status = query_status(&self.config);
            self.update_tray_icon();
        }

        let autostart_on = autostart::is_enabled();

        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }

            let header = if self.switching {
                format!("网卡: {}  [正在切换...]", self.status.interface_name)
            } else {
                format!(
                    "网卡: {}  [{}]",
                    self.status.interface_name,
                    self.status.mode.display_name()
                )
            };
            let ip_gw = format!(
                "IP: {}   网关: {}",
                self.status.ipv4.as_deref().unwrap_or("-"),
                self.status.gateway.as_deref().unwrap_or("-")
            );
            let dns_line = format!(
                "DNS: {}",
                if self.status.dns.is_empty() {
                    "-".to_string()
                } else {
                    self.status.dns.join(", ")
                }
            );

            append_item(menu, MF_STRING | MF_GRAYED, 0, &header);
            append_item(menu, MF_STRING | MF_GRAYED, 0, &ip_gw);
            append_item(menu, MF_STRING | MF_GRAYED, 0, &dns_line);
            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

            let toggle_label = if self.switching {
                "正在切换网络模式...".to_string()
            } else {
                match self.status.mode {
                    NetworkMode::Static => "一键切换 → DHCP 自动模式".to_string(),
                    _ => format!(
                        "一键切换 → 固定网关 ({})",
                        self.config.static_cfg.gateway
                    ),
                }
            };
            let switch_flags = if self.switching {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            append_item(menu, switch_flags, CMD_TOGGLE as usize, &toggle_label);
            SetMenuDefaultItem(menu, CMD_TOGGLE, 0);

            let static_flags = switch_flags
                | if self.status.mode == NetworkMode::Static {
                    MF_CHECKED
                } else {
                    0
                };
            let dhcp_flags = switch_flags
                | if self.status.mode == NetworkMode::Dhcp {
                    MF_CHECKED
                } else {
                    0
                };
            append_item(
                menu,
                static_flags,
                CMD_SET_STATIC as usize,
                &format!("切换到 固定网关模式 ({})", self.config.static_cfg.gateway),
            );
            append_item(
                menu,
                dhcp_flags,
                CMD_SET_DHCP as usize,
                "切换到 DHCP 自动模式",
            );

            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            append_item(menu, MF_STRING, CMD_OPEN_GUI as usize, "网卡与网络配置 (GUI)...");
            append_item(menu, MF_STRING, CMD_REFRESH as usize, "立即刷新状态");
            append_item(
                menu,
                MF_STRING,
                CMD_OPEN_CONFIG as usize,
                "打开配置文件 (config.json)",
            );
            append_item(menu, MF_STRING, CMD_RELOAD_CONFIG as usize, "重新加载配置");

            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            let auto_flags = MF_STRING | if autostart_on { MF_CHECKED } else { 0 };
            append_item(
                menu,
                auto_flags,
                CMD_TOGGLE_AUTOSTART as usize,
                "开机自启动 (任务计划免 UAC)",
            );

            AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            append_item(menu, MF_STRING, CMD_EXIT as usize, "退出");

            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(self.hwnd);

            let cmd = TrackPopupMenuEx(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                self.hwnd,
                ptr::null(),
            ) as u32;

            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);

            match cmd {
                CMD_TOGGLE => self.toggle_mode(),
                CMD_SET_STATIC => self.start_switch(SwitchTarget::Static),
                CMD_SET_DHCP => self.start_switch(SwitchTarget::Dhcp),
                CMD_OPEN_GUI => {
                    if let Err(e) = launch_gui_process() {
                        self.show_notification("打开配置界面失败", &e, true);
                    }
                }
                CMD_REFRESH => self.refresh_status(),
                CMD_OPEN_CONFIG => {
                    if let Err(e) = open_file_in_shell(&self.config_path) {
                        self.show_notification("打开配置失败", &e, true);
                    }
                }
                CMD_RELOAD_CONFIG => self.reload_config(),
                CMD_TOGGLE_AUTOSTART => self.toggle_autostart(),
                CMD_EXIT => {
                    DestroyWindow(self.hwnd);
                }
                _ => {}
            }
        }
    }
}

unsafe fn append_item(menu: *mut std::ffi::c_void, flags: u32, id: usize, text: &str) {
    let w = to_wide_null(text);
    unsafe {
        AppendMenuW(menu, flags, id, w.as_ptr());
    }
}

pub fn run(config: Config, config_path: PathBuf) -> Result<(), String> {
    let class_name = to_wide_null(TRAY_WINDOW_CLASS);
    let window_title = to_wide_null("NetSwitcher");
    let taskbar_created_w = to_wide_null("TaskbarCreated");

    unsafe {
        let hinstance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };

        if RegisterClassW(&wc) == 0 {
            return Err("注册隐藏窗口类失败".to_string());
        }

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinstance,
            ptr::null(),
        );
        if hwnd.is_null() {
            return Err("创建托盘消息窗口失败".to_string());
        }

        let taskbar_created_msg = RegisterWindowMessageW(taskbar_created_w.as_ptr());
        let initial_status = query_status(&config);
        let watcher = NetChangeWatcher::start(hwnd, WM_APP_NET_CHANGED);

        let mut state = Box::new(AppState {
            hwnd,
            config_path,
            config,
            status: initial_status,
            switching: false,
            current_visual: None,
            current_icon: None,
            taskbar_created_msg,
            _watcher: watcher,
        });

        state.add_tray_icon()?;
        state.reset_poll_timer();

        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;

    if !ptr.is_null() {
        let state = unsafe { &mut *ptr };

        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
            let _ = state.add_tray_icon();
            return 0;
        }

        match msg {
            WM_APP_TRAY => {
                let mouse_msg = (lparam & 0xFFFF) as u32;
                match mouse_msg {
                    WM_LBUTTONUP => {
                        if state.config.ui.left_click_toggle {
                            state.toggle_mode();
                        }
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        state.show_context_menu();
                    }
                    _ => {}
                }
                return 0;
            }
            WM_APP_NET_CHANGED => {
                unsafe {
                    KillTimer(hwnd, TIMER_DEBOUNCE_ID);
                    SetTimer(hwnd, TIMER_DEBOUNCE_ID, 500, None);
                }
                return 0;
            }
            WM_APP_SWITCH_DONE => {
                if lparam != 0 {
                    let res = unsafe { Box::from_raw(lparam as *mut SwitchResult) };
                    state.handle_switch_done(*res);
                }
                return 0;
            }
            WM_APP_RELOAD_CONFIG => {
                if let Ok(new_cfg) = load_config(&state.config_path) {
                    state.config = new_cfg;
                    state.reset_poll_timer();
                    state.refresh_status();
                }
                return 0;
            }
            WM_TIMER => {
                if wparam == TIMER_DEBOUNCE_ID {
                    unsafe {
                        KillTimer(hwnd, TIMER_DEBOUNCE_ID);
                    }
                    if !state.switching {
                        state.refresh_status();
                    }
                } else if wparam == TIMER_POLL_ID && !state.switching {
                    state.refresh_status();
                }
                return 0;
            }
            WM_DESTROY => {
                unsafe {
                    KillTimer(hwnd, TIMER_POLL_ID);
                    KillTimer(hwnd, TIMER_DEBOUNCE_ID);
                }
                state.remove_tray_icon();
                unsafe {
                    PostQuitMessage(0);
                }
                return 0;
            }
            WM_NCDESTROY => unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
                return 0;
            },
            _ => {}
        }
    } else if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
