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
    IP_ADAPTER_IPV6_ENABLED, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR, SOCKADDR_IN};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{Config, Ipv6Setting, NetworkProfile, ProfileMode};

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
            NetworkMode::Static => "静态 IP",
            NetworkMode::StaticCustom => "静态 IP (未匹配 Profile)",
            NetworkMode::Dhcp => "DHCP 自动",
            NetworkMode::Disconnected => "未连接 / 异常",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkStatus {
    pub interface_name: String,
    pub found: bool,
    pub is_up: bool,
    pub dhcp_enabled: bool,
    pub ipv6_enabled: bool,
    pub ipv4: Option<String>,
    pub subnet_mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub mode: NetworkMode,
    pub matched_profile_id: Option<String>,
    pub matched_profile_name: Option<String>,
}

impl NetworkStatus {
    pub fn display_label(&self) -> String {
        if let Some(ref name) = self.matched_profile_name {
            format!("{} ({})", name, self.mode.display_name())
        } else {
            self.mode.display_name().to_string()
        }
    }

    pub fn tooltip_text(&self, switching_to: Option<&str>) -> String {
        if let Some(target_name) = switching_to {
            return format!("{}\n正在切换到「{}」...", self.interface_name, target_name);
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
        let v6 = if self.ipv6_enabled { "开" } else { "关" };
        format!(
            "{} [{}]\nIP: {} (IPv6:{})\nGW: {}\nDNS: {}",
            self.interface_name,
            self.display_label(),
            ip,
            v6,
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
        let v6 = if self.ipv6_enabled { "开启" } else { "关闭" };
        format!("IP: {} | 网关: {} | DNS: {} | IPv6: {}", ip, gw, dns, v6)
    }
}

#[derive(Debug, Clone)]
pub struct AdapterSummary {
    pub friendly_name: String,
    pub description: String,
    pub is_up: bool,
    pub dhcp_enabled: bool,
    pub ipv6_enabled: bool,
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
                AF_UNSPEC as u32,
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
            b.is_up.cmp(&a.is_up).then_with(|| {
                a.friendly_name
                    .to_lowercase()
                    .cmp(&b.friendly_name.to_lowercase())
            })
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
    let ipv6_enabled = (flags & IP_ADAPTER_IPV6_ENABLED) != 0;

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
        ipv6_enabled,
        ipv4,
        subnet_mask,
        gateway,
        dns,
    }
}

pub fn query_status(config: &Config) -> NetworkStatus {
    query_interface_status(&config.active_interface, &config.profiles)
}

pub fn query_interface_status(
    interface_name: &str,
    profiles: &[NetworkProfile],
) -> NetworkStatus {
    let target_name = interface_name.trim();
    let adapters = list_adapters();

    let matched_adapter = adapters
        .iter()
        .find(|a| a.friendly_name == target_name)
        .or_else(|| {
            adapters
                .iter()
                .find(|a| a.friendly_name.eq_ignore_ascii_case(target_name))
        });

    let Some(adapter) = matched_adapter else {
        return NetworkStatus {
            interface_name: target_name.to_string(),
            found: false,
            is_up: false,
            dhcp_enabled: false,
            ipv6_enabled: false,
            ipv4: None,
            subnet_mask: None,
            gateway: None,
            dns: Vec::new(),
            mode: NetworkMode::Disconnected,
            matched_profile_id: None,
            matched_profile_name: None,
        };
    };

    let (mode, matched_profile) = match_adapter_profile(adapter, profiles);

    NetworkStatus {
        interface_name: adapter.friendly_name.clone(),
        found: true,
        is_up: adapter.is_up,
        dhcp_enabled: adapter.dhcp_enabled,
        ipv6_enabled: adapter.ipv6_enabled,
        ipv4: adapter.ipv4.clone(),
        subnet_mask: adapter.subnet_mask.clone(),
        gateway: adapter.gateway.clone(),
        dns: adapter.dns.clone(),
        mode,
        matched_profile_id: matched_profile.map(|p| p.id.clone()),
        matched_profile_name: matched_profile.map(|p| p.name.clone()),
    }
}

fn match_adapter_profile<'a>(
    adapter: &AdapterSummary,
    profiles: &'a [NetworkProfile],
) -> (NetworkMode, Option<&'a NetworkProfile>) {
    if !adapter.is_up {
        return (NetworkMode::Disconnected, None);
    }

    let iface_profiles: Vec<&NetworkProfile> = profiles
        .iter()
        .filter(|p| p.interface_name.eq_ignore_ascii_case(&adapter.friendly_name))
        .collect();

    if adapter.dhcp_enabled {
        let matched = iface_profiles.into_iter().find(|p| p.mode.is_dhcp());
        return (NetworkMode::Dhcp, matched);
    }

    // Prefer exact static match including DNS, then fall back to IP + gateway match
    let mut ip_gw_match: Option<&NetworkProfile> = None;
    for p in iface_profiles {
        if let ProfileMode::Static {
            ip, gateway, dns, ..
        } = &p.mode
        {
            if adapter.ipv4.as_deref() == Some(ip.as_str())
                && adapter.gateway.as_deref() == Some(gateway.as_str())
            {
                if dns.is_empty() || adapter.dns == *dns {
                    return (NetworkMode::Static, Some(p));
                }
                if ip_gw_match.is_none() {
                    ip_gw_match = Some(p);
                }
            }
        }
    }

    if let Some(p) = ip_gw_match {
        (NetworkMode::Static, Some(p))
    } else {
        (NetworkMode::StaticCustom, None)
    }
}

pub fn pick_next_profile<'a>(
    config: &'a Config,
    current: &NetworkStatus,
) -> Option<&'a NetworkProfile> {
    let profiles = config.active_profiles();
    if profiles.is_empty() {
        return None;
    }
    if profiles.len() == 1 {
        return Some(profiles[0]);
    }

    if let Some(ref active_id) = current.matched_profile_id {
        if let Some(idx) = profiles.iter().position(|p| p.id == *active_id) {
            return Some(profiles[(idx + 1) % profiles.len()]);
        }
    }

    // If current state didn't match a named profile, prefer switching to opposite DHCP/Static mode
    profiles
        .iter()
        .copied()
        .find(|p| p.mode.is_dhcp() != current.dhcp_enabled)
        .or_else(|| Some(profiles[0]))
}

pub fn apply_profile(
    profile: &NetworkProfile,
    all_profiles: &[NetworkProfile],
) -> Result<NetworkStatus, String> {
    let status = match &profile.mode {
        ProfileMode::Static {
            ip,
            subnet_mask,
            gateway,
            gateway_metric,
            dns,
        } => apply_static_mode(
            &profile.interface_name,
            ip,
            subnet_mask,
            gateway,
            *gateway_metric,
            dns,
            all_profiles,
        )?,
        ProfileMode::Dhcp { renew_on_switch } => {
            apply_dhcp_mode(&profile.interface_name, *renew_on_switch, all_profiles)?
        }
    };

    match profile.ipv6 {
        Ipv6Setting::Keep => Ok(status),
        Ipv6Setting::Enable => {
            if status.ipv6_enabled {
                Ok(status)
            } else {
                set_interface_ipv6(&profile.interface_name, true, all_profiles)
            }
        }
        Ipv6Setting::Disable => {
            if !status.ipv6_enabled {
                Ok(status)
            } else {
                set_interface_ipv6(&profile.interface_name, false, all_profiles)
            }
        }
    }
}

pub fn set_interface_ipv6(
    interface_name: &str,
    enable: bool,
    all_profiles: &[NetworkProfile],
) -> Result<NetworkStatus, String> {
    let escaped = interface_name.replace('\'', "''");
    let cmdlet = if enable {
        "Enable-NetAdapterBinding"
    } else {
        "Disable-NetAdapterBinding"
    };
    let ps_cmd = format!(
        "{} -Name '{}' -ComponentID ms_tcpip6 -Confirm:$false -ErrorAction Stop | Out-Null",
        cmdlet, escaped
    );
    let args = vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-WindowStyle".to_string(),
        "Hidden".to_string(),
        "-Command".to_string(),
        ps_cmd,
    ];
    let res = run_hidden_command("powershell.exe", &args)?;
    if !res.success {
        return Err(format!(
            "{} 网卡「{}」IPv6 失败 (code={:?}): {}",
            if enable { "开启" } else { "关闭" },
            interface_name,
            res.code,
            res.output
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let st = query_interface_status(interface_name, all_profiles);
        if st.ipv6_enabled == enable || Instant::now() >= deadline {
            return Ok(st);
        }
    }
}

fn apply_static_mode(
    iface: &str,
    ip: &str,
    subnet_mask: &str,
    gateway: &str,
    gateway_metric: u32,
    dns: &[String],
    all_profiles: &[NetworkProfile],
) -> Result<NetworkStatus, String> {
    let mut addr_args = vec![
        "interface".to_string(),
        "ipv4".to_string(),
        "set".to_string(),
        "address".to_string(),
        format!("name={}", iface),
        "source=static".to_string(),
        format!("address={}", ip),
        format!("mask={}", subnet_mask),
        format!("gateway={}", gateway),
    ];
    if gateway_metric > 0 {
        addr_args.push(format!("gwmetric={}", gateway_metric));
    }

    let addr_out = run_hidden_command("netsh.exe", &addr_args)?;
    if !addr_out.success {
        return Err(format!(
            "设置静态 IP/网关失败 (code={:?}): {}",
            addr_out.code, addr_out.output
        ));
    }

    if dns.is_empty() {
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
            format!("address={}", dns[0]),
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

        for (idx, dns_ip) in dns.iter().enumerate().skip(1) {
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
        let st = query_interface_status(iface, all_profiles);
        if !st.dhcp_enabled
            && st.ipv4.as_deref() == Some(ip)
            && st.gateway.as_deref() == Some(gateway)
        {
            return Ok(st);
        }
        if Instant::now() >= deadline {
            if !st.dhcp_enabled {
                return Ok(st);
            }
            return Err(format!(
                "已执行切换命令，但网卡状态未变为静态模式 ({})",
                st.summary_line()
            ));
        }
    }
}

fn apply_dhcp_mode(
    iface: &str,
    renew_on_switch: bool,
    all_profiles: &[NetworkProfile],
) -> Result<NetworkStatus, String> {
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

    if renew_on_switch {
        let renew_args = vec!["/renew".to_string(), iface.to_string()];
        let _ = run_hidden_command("ipconfig.exe", &renew_args);
    }

    let deadline = Instant::now() + Duration::from_secs(7);
    let mut last_status = query_interface_status(iface, all_profiles);
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
        last_status = query_interface_status(iface, all_profiles);
    }

    if last_status.dhcp_enabled {
        return Ok(last_status);
    }

    Err(format!(
        "切换 DHCP 模式失败。address(code={:?}): {}; dns(code={:?}): {}",
        addr_res.code, addr_res.output, dns_res.code, dns_res.output
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_between_multiple_profiles() {
        let cfg = Config {
            version: 2,
            active_interface: "WLAN".to_string(),
            profiles: vec![
                NetworkProfile {
                    id: "p1".to_string(),
                    name: "静态 A".to_string(),
                    interface_name: "WLAN".to_string(),
                    mode: ProfileMode::Static {
                        ip: "192.168.1.10".to_string(),
                        subnet_mask: "255.255.255.0".to_string(),
                        gateway: "192.168.1.2".to_string(),
                        gateway_metric: 0,
                        dns: vec!["192.168.1.1".to_string()],
                    },
                    ipv6: Ipv6Setting::Disable,
                },
                NetworkProfile {
                    id: "p2".to_string(),
                    name: "静态 B".to_string(),
                    interface_name: "WLAN".to_string(),
                    mode: ProfileMode::Static {
                        ip: "192.168.1.20".to_string(),
                        subnet_mask: "255.255.255.0".to_string(),
                        gateway: "192.168.1.3".to_string(),
                        gateway_metric: 0,
                        dns: vec!["192.168.1.1".to_string()],
                    },
                    ipv6: Ipv6Setting::Keep,
                },
                NetworkProfile {
                    id: "p3".to_string(),
                    name: "DHCP".to_string(),
                    interface_name: "WLAN".to_string(),
                    mode: ProfileMode::Dhcp {
                        renew_on_switch: true,
                    },
                    ipv6: Ipv6Setting::Enable,
                },
            ],
            ui: Default::default(),
        };

        let status = NetworkStatus {
            interface_name: "WLAN".to_string(),
            found: true,
            is_up: true,
            dhcp_enabled: false,
            ipv6_enabled: false,
            ipv4: Some("192.168.1.10".to_string()),
            subnet_mask: Some("255.255.255.0".to_string()),
            gateway: Some("192.168.1.2".to_string()),
            dns: vec!["192.168.1.1".to_string()],
            mode: NetworkMode::Static,
            matched_profile_id: Some("p1".to_string()),
            matched_profile_name: Some("静态 A".to_string()),
        };

        let next = pick_next_profile(&cfg, &status).unwrap();
        assert_eq!(next.id, "p2");
    }
}
