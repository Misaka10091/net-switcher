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

    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let lock = match winutil::acquire_single_instance(TRAY_MUTEX_NAME) {
        Ok(winutil::InstanceLock::Acquired(guard)) => guard,
        Ok(winutil::InstanceLock::AlreadyRunning) => return,
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
    let cfg = match config::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            winutil::show_error_box("Net Switcher 配置错误", &e);
            return;
        }
    };

    if let Err(e) = tray::run(cfg, config_path) {
        winutil::show_error_box("Net Switcher 运行错误", &e);
    }

    drop(lock);
}

fn run_gui_mode() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let _gui_lock = match winutil::acquire_single_instance(GUI_MUTEX_NAME) {
        Ok(winutil::InstanceLock::Acquired(guard)) => guard,
        Ok(winutil::InstanceLock::AlreadyRunning) => return,
        Err(e) => {
            winutil::show_error_box("Net Switcher GUI 启动失败", &e);
            return;
        }
    };

    let config_path = config::resolve_config_path();
    let cfg = config::load_config(&config_path).unwrap_or_else(|_| {
        let first_adapter = net::list_adapters()
            .into_iter()
            .next()
            .map(|a| a.friendly_name)
            .unwrap_or_else(|| "WLAN".to_string());
        config::Config {
            interface_name: first_adapter,
            static_cfg: config::StaticConfig {
                ip: "192.168.1.100".to_string(),
                subnet_mask: "255.255.255.0".to_string(),
                gateway: "192.168.1.2".to_string(),
                gateway_metric: 0,
                dns: vec!["192.168.1.1".to_string()],
            },
            dhcp: config::DhcpConfig::default(),
            ui: config::UiConfig::default(),
        }
    });

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
            println!("Mode        : {}", st.mode.display_name());
            println!("DHCP Enabled: {}", st.dhcp_enabled);
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
            println!("Elevated    : {}", winutil::is_elevated());
            println!("Autostart   : {}", autostart::is_enabled());
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
