use std::ffi::c_void;
use std::net::Ipv4Addr;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, HANDLE, HWND, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_INCLUDE_PREFIX,
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_DHCP_ENABLED,
    MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Static,
    StaticCustom,
    Dhcp,
    Disconnected,
}

impl NetworkMode {
    pub fn display_name(self) -> &'static str {
        match self {
            NetworkMode::Static => "固定网关模式",
            NetworkMode::StaticCustom => "静态模式 (自定义)",
            NetworkMode::Dhcp => "DHCP 自动模式",
            NetworkMode::Disconnected => "未连接 / 异常",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchTarget {
    Static,
    Dhcp,
}

impl SwitchTarget {
    pub fn display_name(self) -> &'static str {
        match self {
            SwitchTarget::Static => "固定网关模式",
            SwitchTarget::Dhcp => "DHCP 自动模式",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub interface_name: String,
    pub found: bool,
    pub is_up: bool,
    pub dhcp_enabled: bool,
    pub ipv4: Option<String>,
    pub subnet_mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub mode: NetworkMode,
}

impl NetworkStatus {
    pub fn tooltip_text(&self, switching: bool) -> String {
        if switching {
            return format!("{}\n正在切换网络模式...", self.interface_name);
        }
        if !self.found {
            return format!("{}\n未找到该网卡", self.interface_name);
        }
        let ip = self.ipv4.as_deref().unwrap_or("-");
        let gw = self.gateway.as_deref().unwrap_or("-");
        let dns = if self.dns.is_empty() {
            "-".to_string()
        } else {
            self.dns.join(",")
        };
        format!(
            "{} [{}]\nIP: {}\nGW: {}\nDNS: {}",
            self.interface_name,
            self.mode.display_name(),
            ip,
            gw,
            dns
        )
    }

    pub fn summary_line(&self) -> String {
        let ip = self.ipv4.as_deref().unwrap_or("无");
        let gw = self.gateway.as_deref().unwrap_or("无");
        let dns = if self.dns.is_empty() {
            "无".to_string()
        } else {
            self.dns.join(", ")
        };
        format!("IP: {} | 网关: {} | DNS: {}", ip, gw, dns)
    }
}

#[derive(Debug, Clone)]
pub struct AdapterSummary {
    pub friendly_name: String,
    pub description: String,
    pub is_up: bool,
    pub dhcp_enabled: bool,
    pub ipv4: Option<String>,
    pub subnet_mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

pub fn list_adapters() -> Vec<AdapterSummary> {
    let mut size: u32 = 16 * 1024;
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_INCLUDE_PREFIX;
    let mut buffer = vec![0u8; size as usize];

    for _ in 0..3 {
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_INET as u32,
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &mut size,
            )
        };
        if ret == ERROR_BUFFER_OVERFLOW {
            buffer.resize(size as usize, 0);
            continue;
        }
        if ret != NO_ERROR {
            return Vec::new();
        }

        let mut adapters = Vec::new();
        let mut curr = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !curr.is_null() {
            let adapter = unsafe { &*curr };
            if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                adapters.push(extract_adapter_summary(adapter));
            }
            curr = adapter.Next;
        }

        adapters.sort_by(|a, b| {
            b.is_up
                .cmp(&a.is_up)
                .then_with(|| a.friendly_name.to_lowercase().cmp(&b.friendly_name.to_lowercase()))
        });
        return adapters;
    }

    Vec::new()
}

fn extract_adapter_summary(adapter: &IP_ADAPTER_ADDRESSES_LH) -> AdapterSummary {
    let friendly_name = unsafe { pwstr_to_string(adapter.FriendlyName) };
    let description = unsafe { pwstr_to_string(adapter.Description) };
    let is_up = adapter.OperStatus == IfOperStatusUp;
    let flags = unsafe { adapter.Anonymous2.Flags };
    let dhcp_enabled = (flags & IP_ADAPTER_DHCP_ENABLED) != 0;

    let mut ipv4: Option<String> = None;
    let mut subnet_mask: Option<String> = None;
    let mut uni = adapter.FirstUnicastAddress;
    while !uni.is_null() {
        let u = unsafe { &*uni };
        if let Some(ip) = sockaddr_to_ipv4(u.Address.lpSockaddr) {
            ipv4 = Some(ip.to_string());
            subnet_mask = Some(prefix_len_to_mask(u.OnLinkPrefixLength).to_string());
            break;
        }
        uni = u.Next;
    }

    let mut gateway: Option<String> = None;
    let mut gw = adapter.FirstGatewayAddress;
    while !gw.is_null() {
        let g = unsafe { &*gw };
        if let Some(ip) = sockaddr_to_ipv4(g.Address.lpSockaddr) {
            gateway = Some(ip.to_string());
            break;
        }
        gw = g.Next;
    }

    let mut dns: Vec<String> = Vec::new();
    let mut dns_ptr = adapter.FirstDnsServerAddress;
    while !dns_ptr.is_null() {
        let d = unsafe { &*dns_ptr };
        if let Some(ip) = sockaddr_to_ipv4(d.Address.lpSockaddr) {
            let s = ip.to_string();
            if !dns.contains(&s) {
                dns.push(s);
            }
        }
        dns_ptr = d.Next;
    }

    AdapterSummary {
        friendly_name,
        description,
        is_up,
        dhcp_enabled,
        ipv4,
        subnet_mask,
        gateway,
        dns,
    }
}

pub fn query_status(config: &Config) -> NetworkStatus {
    let target_name = config.interface_name.trim();
    let mut size: u32 = 16 * 1024;
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_INCLUDE_PREFIX;

    let mut buffer = vec![0u8; size as usize];
    for _ in 0..3 {
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_INET as u32,
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &mut size,
            )
        };
        if ret == ERROR_BUFFER_OVERFLOW {
            buffer.resize(size as usize, 0);
            continue;
        }
        if ret != NO_ERROR {
            return disconnected_status(target_name, false);
        }

        let mut curr = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let mut fallback_match: Option<*const IP_ADAPTER_ADDRESSES_LH> = None;

        while !curr.is_null() {
            let adapter = unsafe { &*curr };
            let friendly = unsafe { pwstr_to_string(adapter.FriendlyName) };
            if friendly == target_name {
                return extract_adapter_status(adapter, config);
            }
            if fallback_match.is_none() && friendly.eq_ignore_ascii_case(target_name) {
                fallback_match = Some(curr);
            }
            curr = adapter.Next;
        }

        if let Some(ptr_match) = fallback_match {
            let adapter = unsafe { &*ptr_match };
            return extract_adapter_status(adapter, config);
        }

        return disconnected_status(target_name, false);
    }

    disconnected_status(target_name, false)
}

fn disconnected_status(interface_name: &str, found: bool) -> NetworkStatus {
    NetworkStatus {
        interface_name: interface_name.to_string(),
        found,
        is_up: false,
        dhcp_enabled: false,
        ipv4: None,
        subnet_mask: None,
        gateway: None,
        dns: Vec::new(),
        mode: NetworkMode::Disconnected,
    }
}

fn extract_adapter_status(adapter: &IP_ADAPTER_ADDRESSES_LH, config: &Config) -> NetworkStatus {
    let interface_name = unsafe { pwstr_to_string(adapter.FriendlyName) };
    let is_up = adapter.OperStatus == IfOperStatusUp;
    let flags = unsafe { adapter.Anonymous2.Flags };
    let dhcp_enabled = (flags & IP_ADAPTER_DHCP_ENABLED) != 0;

    let mut ipv4: Option<String> = None;
    let mut subnet_mask: Option<String> = None;

    let mut uni = adapter.FirstUnicastAddress;
    while !uni.is_null() {
        let u = unsafe { &*uni };
        if let Some(ip) = sockaddr_to_ipv4(u.Address.lpSockaddr) {
            ipv4 = Some(ip.to_string());
            subnet_mask = Some(prefix_len_to_mask(u.OnLinkPrefixLength).to_string());
            break;
        }
        uni = u.Next;
    }

    let mut gateway: Option<String> = None;
    let mut gw = adapter.FirstGatewayAddress;
    while !gw.is_null() {
        let g = unsafe { &*gw };
        if let Some(ip) = sockaddr_to_ipv4(g.Address.lpSockaddr) {
            gateway = Some(ip.to_string());
            break;
        }
        gw = g.Next;
    }

    let mut dns: Vec<String> = Vec::new();
    let mut dns_ptr = adapter.FirstDnsServerAddress;
    while !dns_ptr.is_null() {
        let d = unsafe { &*dns_ptr };
        if let Some(ip) = sockaddr_to_ipv4(d.Address.lpSockaddr) {
            let s = ip.to_string();
            if !dns.contains(&s) {
                dns.push(s);
            }
        }
        dns_ptr = d.Next;
    }

    let mode = if !is_up {
        NetworkMode::Disconnected
    } else if dhcp_enabled {
        NetworkMode::Dhcp
    } else if ipv4.as_deref() == Some(config.static_cfg.ip.as_str())
        && gateway.as_deref() == Some(config.static_cfg.gateway.as_str())
    {
        NetworkMode::Static
    } else {
        NetworkMode::StaticCustom
    };

    NetworkStatus {
        interface_name,
        found: true,
        is_up,
        dhcp_enabled,
        ipv4,
        subnet_mask,
        gateway,
        dns,
        mode,
    }
}

fn sockaddr_to_ipv4(sockaddr: *const SOCKADDR) -> Option<Ipv4Addr> {
    if sockaddr.is_null() {
        return None;
    }
    unsafe {
        if (*sockaddr).sa_family != AF_INET {
            return None;
        }
        let sin = &*(sockaddr.cast::<SOCKADDR_IN>());
        let raw = sin.sin_addr.S_un.S_addr;
        Some(Ipv4Addr::from(u32::from_be(raw)))
    }
}

fn prefix_len_to_mask(prefix_len: u8) -> Ipv4Addr {
    if prefix_len == 0 {
        Ipv4Addr::new(0, 0, 0, 0)
    } else if prefix_len >= 32 {
        Ipv4Addr::new(255, 255, 255, 255)
    } else {
        let mask = (!0u32) << (32 - prefix_len);
        Ipv4Addr::from(mask)
    }
}

unsafe fn pwstr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

pub fn switch_mode(config: &Config, target: SwitchTarget) -> Result<NetworkStatus, String> {
    match target {
        SwitchTarget::Static => apply_static_mode(config),
        SwitchTarget::Dhcp => apply_dhcp_mode(config),
    }
}

fn apply_static_mode(config: &Config) -> Result<NetworkStatus, String> {
    let iface = &config.interface_name;
    let sc = &config.static_cfg;

    let mut addr_args = vec![
        "interface".to_string(),
        "ipv4".to_string(),
        "set".to_string(),
        "address".to_string(),
        format!("name={}", iface),
        "source=static".to_string(),
        format!("address={}", sc.ip),
        format!("mask={}", sc.subnet_mask),
        format!("gateway={}", sc.gateway),
    ];
    if sc.gateway_metric > 0 {
        addr_args.push(format!("gwmetric={}", sc.gateway_metric));
    }

    let addr_out = run_hidden_command("netsh.exe", &addr_args)?;
    if !addr_out.success {
        return Err(format!(
            "设置静态 IP/网关失败 (code={:?}): {}",
            addr_out.code, addr_out.output
        ));
    }

    if sc.dns.is_empty() {
        let dns_args = vec![
            "interface".to_string(),
            "ipv4".to_string(),
            "set".to_string(),
            "dns".to_string(),
            format!("name={}", iface),
            "source=dhcp".to_string(),
        ];
        let _ = run_hidden_command("netsh.exe", &dns_args);
    } else {
        let primary_args = vec![
            "interface".to_string(),
            "ipv4".to_string(),
            "set".to_string(),
            "dns".to_string(),
            format!("name={}", iface),
            "source=static".to_string(),
            format!("address={}", sc.dns[0]),
            "register=primary".to_string(),
            "validate=no".to_string(),
        ];
        let dns_out = run_hidden_command("netsh.exe", &primary_args)?;
        if !dns_out.success {
            return Err(format!(
                "设置静态主 DNS 失败 (code={:?}): {}",
                dns_out.code, dns_out.output
            ));
        }

        for (idx, dns_ip) in sc.dns.iter().enumerate().skip(1) {
            let add_args = vec![
                "interface".to_string(),
                "ipv4".to_string(),
                "add".to_string(),
                "dns".to_string(),
                format!("name={}", iface),
                format!("address={}", dns_ip),
                format!("index={}", idx + 1),
                "validate=no".to_string(),
            ];
            let add_out = run_hidden_command("netsh.exe", &add_args)?;
            if !add_out.success {
                return Err(format!(
                    "设置静态备用 DNS {} 失败 (code={:?}): {}",
                    dns_ip, add_out.code, add_out.output
                ));
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let st = query_status(config);
        if !st.dhcp_enabled
            && st.ipv4.as_deref() == Some(sc.ip.as_str())
            && st.gateway.as_deref() == Some(sc.gateway.as_str())
        {
            return Ok(st);
        }
        if Instant::now() >= deadline {
            if !st.dhcp_enabled {
                return Ok(st);
            }
            return Err(format!(
                "已执行切换命令，但网卡状态未变为固定网关 ({})",
                st.summary_line()
            ));
        }
    }
}

fn apply_dhcp_mode(config: &Config) -> Result<NetworkStatus, String> {
    let iface = &config.interface_name;

    let addr_args = vec![
        "interface".to_string(),
        "ipv4".to_string(),
        "set".to_string(),
        "address".to_string(),
        format!("name={}", iface),
        "source=dhcp".to_string(),
    ];
    let addr_res = run_hidden_command("netsh.exe", &addr_args)?;

    let dns_args = vec![
        "interface".to_string(),
        "ipv4".to_string(),
        "set".to_string(),
        "dns".to_string(),
        format!("name={}", iface),
        "source=dhcp".to_string(),
    ];
    let dns_res = run_hidden_command("netsh.exe", &dns_args)?;

    if config.dhcp.renew_on_switch {
        let renew_args = vec!["/renew".to_string(), iface.clone()];
        let _ = run_hidden_command("ipconfig.exe", &renew_args);
    }

    let deadline = Instant::now() + Duration::from_secs(7);
    let mut last_status = query_status(config);
    while Instant::now() < deadline {
        if last_status.dhcp_enabled
            && last_status
                .ipv4
                .as_deref()
                .map(|ip| !ip.starts_with("169.254."))
                .unwrap_or(false)
            && last_status.gateway.is_some()
        {
            return Ok(last_status);
        }
        std::thread::sleep(Duration::from_millis(350));
        last_status = query_status(config);
    }

    if last_status.dhcp_enabled {
        return Ok(last_status);
    }

    Err(format!(
        "切换 DHCP 模式失败。address(code={:?}): {}; dns(code={:?}): {}",
        addr_res.code, addr_res.output, dns_res.code, dns_res.output
    ))
}

struct CmdOutput {
    success: bool,
    code: Option<i32>,
    output: String,
}

fn run_hidden_command(program: &str, args: &[String]) -> Result<CmdOutput, String> {
    let out = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("无法启动 {}: {}", program, e))?;

    let mut combined = String::new();
    let stdout = crate::winutil::decode_system_bytes(&out.stdout)
        .trim()
        .to_string();
    let stderr = crate::winutil::decode_system_bytes(&out.stderr)
        .trim()
        .to_string();
    if !stdout.is_empty() {
        combined.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push_str(" | ");
        }
        combined.push_str(&stderr);
    }

    Ok(CmdOutput {
        success: out.status.success(),
        code: out.status.code(),
        output: combined,
    })
}

struct WatcherCtx {
    hwnd: HWND,
    msg: u32,
}

pub struct NetChangeWatcher {
    handle: HANDLE,
    ctx: *mut WatcherCtx,
}

impl NetChangeWatcher {
    pub fn start(hwnd: HWND, msg: u32) -> Option<Self> {
        let ctx = Box::into_raw(Box::new(WatcherCtx { hwnd, msg }));
        let mut handle: HANDLE = ptr::null_mut();
        let ret = unsafe {
            NotifyIpInterfaceChange(
                AF_INET,
                Some(ip_change_callback),
                ctx.cast::<c_void>(),
                0,
                &mut handle,
            )
        };
        if ret != NO_ERROR || handle.is_null() {
            unsafe {
                drop(Box::from_raw(ctx));
            }
            return None;
        }
        Some(Self { handle, ctx })
    }
}

impl Drop for NetChangeWatcher {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CancelMibChangeNotify2(self.handle);
            }
            self.handle = ptr::null_mut();
        }
        if !self.ctx.is_null() {
            unsafe {
                drop(Box::from_raw(self.ctx));
            }
            self.ctx = ptr::null_mut();
        }
    }
}

unsafe extern "system" fn ip_change_callback(
    caller_context: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    _notification_type: MIB_NOTIFICATION_TYPE,
) {
    if caller_context.is_null() {
        return;
    }
    let ctx = unsafe { &*(caller_context.cast::<WatcherCtx>()) };
    unsafe {
        PostMessageW(ctx.hwnd, ctx.msg, 0, 0);
    }
}
