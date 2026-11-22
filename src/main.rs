#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod gui;
mod icon;
mod net;
mod tray;
mod winutil;

use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

const TRAY_MUTEX_NAME: &str = "Local\\NetSwitcher_SingleInstance_Mutex";
const GUI_MUTEX_NAME: &str = "Local\\NetSwitcher_Gui_Mutex";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--status" || a == "-s") {
        run_cli_status();
        return;
    }

    if args.iter().any(|a| a == "--gui" || a == "-g") {
        run_gui_mode();
        return;
    }

    let no_elevate = args.iter().any(|a| a == "--no-elevate");
    let start_minimized = args.iter().any(|a| a == "--minimized" || a == "--tray-only");

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let lock = match winutil::acquire_single_instance(TRAY_MUTEX_NAME) {
        Ok(winutil::InstanceLock::Acquired(guard)) => guard,
        Ok(winutil::InstanceLock::AlreadyRunning) => {
            // If already running in tray, opening the exe again pops up / focuses the GPUI config window
            if !start_minimized && !winutil::notify_tray_message(tray::TRAY_WINDOW_CLASS, tray::WM_APP_OPEN_GUI) {
                let _ = winutil::launch_gui_process();
            }
            return;
        }
        Err(e) => {
            winutil::show_error_box("Net Switcher 启动失败", &e);
            return;
        }
    };

    if !no_elevate && !winutil::is_elevated() {
        // Release mutex before spawning elevated instance so it can acquire cleanly
        drop(lock);
        let _ = winutil::relaunch_as_admin();
        return;
    }

    let config_path = config::resolve_config_path();
    let cfg = load_or_default_config(&config_path);

    // Pop up GPUI config window on startup unless launched with --minimized (e.g. Windows autostart)
    let open_gui_on_start = !start_minimized;
    if let Err(e) = tray::run(cfg, config_path, open_gui_on_start) {
        winutil::show_error_box("Net Switcher 运行错误", &e);
    }

    drop(lock);
}

fn load_or_default_config(config_path: &std::path::Path) -> config::Config {
    if config_path.exists() {
        match config::load_config(config_path) {
            Ok(c) => return c,
            Err(e) => {
                winutil::show_error_box(
                    "Net Switcher 配置提示",
                    &format!("{}\n\n将使用默认配置启动，你可以在配置界面中重新保存。", e),
                );
            }
        }
    }

    let first_adapter = net::list_adapters()
        .into_iter()
        .next()
        .map(|a| a.friendly_name)
        .unwrap_or_else(|| "WLAN".to_string());
    config::Config::default_for_interface(first_adapter)
}

fn run_gui_mode() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let _gui_lock = match winutil::acquire_single_instance(GUI_MUTEX_NAME) {
        Ok(winutil::InstanceLock::Acquired(guard)) => guard,
        Ok(winutil::InstanceLock::AlreadyRunning) => {
            let _ = winutil::focus_window_by_title(gui::GUI_WINDOW_TITLE);
            return;
        }
        Err(e) => {
            winutil::show_error_box("Net Switcher GUI 启动失败", &e);
            return;
        }
    };

    let config_path = config::resolve_config_path();
    let cfg = load_or_default_config(&config_path);
    gui::run(cfg, config_path);
}

fn run_cli_status() {
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let path = config::resolve_config_path();
    println!("Config Path : {}", path.display());
    match config::load_config(&path) {
        Ok(cfg) => {
            let st = net::query_status(&cfg);
            println!(
                "Interface   : {} (found={}, up={})",
                st.interface_name, st.found, st.is_up
            );
            println!("Active Mode : {}", st.display_label());
            println!("DHCP Enabled: {}", st.dhcp_enabled);
            println!("IPv6 Enabled: {}", st.ipv6_enabled);
            println!("IPv4        : {}", st.ipv4.as_deref().unwrap_or("-"));
            println!("Subnet Mask : {}", st.subnet_mask.as_deref().unwrap_or("-"));
            println!("Gateway     : {}", st.gateway.as_deref().unwrap_or("-"));
            println!(
                "DNS         : {}",
                if st.dns.is_empty() {
                    "-".to_string()
                } else {
                    st.dns.join(", ")
                }
            );
            println!(
                "Profiles    : {} total ({} on {})",
                cfg.profiles.len(),
                cfg.active_profiles().len(),
                cfg.active_interface
            );
            for p in cfg.active_profiles() {
                let mark = if st.matched_profile_id.as_deref() == Some(p.id.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!(
                    "  [{}] {} ({}) - {}",
                    mark,
                    p.name,
                    p.mode.short_label(),
                    p.mode.summary()
                );
            }
            println!("Elevated    : {}", winutil::is_elevated());
            println!("Autostart   : {}", autostart::is_enabled());
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
