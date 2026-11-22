#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod gui;
mod icon;
mod net;
mod tray;
mod winutil;

use std::sync::mpsc;

use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

const SINGLE_INSTANCE_MUTEX: &str = "Local\\NetSwitcher_SingleInstance_Mutex";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--status" || a == "-s") {
        run_cli_status();
        return;
    }

    let no_elevate = args.iter().any(|a| a == "--no-elevate");
    let start_minimized = args.iter().any(|a| a == "--minimized" || a == "--tray-only");

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // Strict single-process mutual exclusion across both normal and elevated launches
    let lock = match winutil::acquire_single_instance(SINGLE_INSTANCE_MUTEX) {
        Ok(winutil::InstanceLock::Acquired(guard)) => guard,
        Ok(winutil::InstanceLock::AlreadyRunning) => {
            if !start_minimized {
                let _ = winutil::notify_tray_message(
                    tray::TRAY_WINDOW_CLASS,
                    tray::WM_APP_OPEN_GUI,
                );
            }
            return;
        }
        Err(e) => {
            winutil::show_error_box("Net Switcher 启动失败", &e);
            return;
        }
    };

    if !no_elevate && !winutil::is_elevated() {
        // Release mutex before spawning elevated single instance
        drop(lock);
        let _ = winutil::relaunch_as_admin();
        return;
    }

    let config_path = config::resolve_config_path();
    let cfg = load_or_default_config(&config_path);

    let (gui_tx, gui_rx) = mpsc::channel();
    let tray_handle = tray::spawn_tray_thread(cfg, config_path.clone(), gui_tx);

    // Main thread runs GPUI event loop; closing the config window keeps this single process resident in tray
    gui::run_app(config_path, gui_rx, !start_minimized);

    let _ = tray_handle.join();
    drop(lock);
}

pub fn load_or_default_config(config_path: &std::path::Path) -> config::Config {
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
            println!("Active      : {}", st.display_label());
            println!(
                "IPv4        : enabled={}, dhcp={}, ip={}, mask={}, gw={}, dns={}",
                st.ipv4_enabled,
                st.dhcp_enabled,
                st.ipv4.as_deref().unwrap_or("-"),
                st.subnet_mask.as_deref().unwrap_or("-"),
                st.gateway.as_deref().unwrap_or("-"),
                if st.dns.is_empty() {
                    "-".to_string()
                } else {
                    st.dns.join(", ")
                }
            );
            println!(
                "IPv6        : enabled={}, ip={}, prefix={}, gw={}, dns={}",
                st.ipv6_enabled,
                st.ipv6.as_deref().unwrap_or("-"),
                st.ipv6_prefix
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                st.ipv6_gateway.as_deref().unwrap_or("-"),
                if st.ipv6_dns.is_empty() {
                    "-".to_string()
                } else {
                    st.ipv6_dns.join(", ")
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
                    "  [{}] {} [{}] - {}",
                    mark,
                    p.name,
                    p.short_mode_summary(),
                    p.detail_summary()
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
