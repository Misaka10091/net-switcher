use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, HANDLE, HWND, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_INCLUDE_PREFIX,
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_DHCP_ENABLED,
    IP_ADAPTER_IPV4_ENABLED, IP_ADAPTER_IPV6_ENABLED, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
    NotifyIpInterfaceChange,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{Config, Ipv4Config, Ipv6Config, NetworkProfile};

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
            NetworkMode::Static => "静态配置",
            NetworkMode::StaticCustom => "自定义配置",
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
    pub ipv4_enabled: bool,
    pub dhcp_enabled: bool,
    pub ipv4: Option<String>,
    pub subnet_mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub ipv6_enabled: bool,
    pub ipv6: Option<String>,
    pub ipv6_prefix: Option<u8>,
    pub ipv6_gateway: Option<String>,
    pub ipv6_dns: Vec<String>,
    pub mode: NetworkMode,
    pub matched_profile_id: Option<String>,
    pub matched_profile_name: Option<String>,
}

impl NetworkStatus {
    pub fn display_label(&self) -> String {
        if let Some(ref name) = self.matched_profile_name {
            name.clone()
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
        let v4 = if !self.ipv4_enabled {
            "已关闭".to_string()
        } else {
            self.ipv4.clone().unwrap_or_else(|| "-".to_string())
        };
        let gw = self.gateway.as_deref().unwrap_or("-");
        let v6 = if !self.ipv6_enabled {
            "关"
        } else {
            "开"
        };
        let dns = if self.dns.is_empty() {
            "-".to_string()
        } else {
            self.dns.join(",")
        };
        format!(
            "{} [{}]\nv4: {} (v6:{})\nGW: {}\nDNS: {}",
            self.interface_name,
            self.display_label(),
            v4,
            v6,
            gw,
            dns
        )
    }

    pub fn summary_line(&self) -> String {
        let v4 = if !self.ipv4_enabled {
            "关闭".to_string()
        } else {
            let ip = self.ipv4.as_deref().unwrap_or("无");
            let gw = self.gateway.as_deref().unwrap_or("无");
            let m = if self.dhcp_enabled { "DHCP" } else { "静态" };
            format!("{} ({}) 网关 {}", ip, m, gw)
        };
        let v6 = if !self.ipv6_enabled {
            "关闭".to_string()
        } else if let Some(ref addr) = self.ipv6 {
            addr.clone()
        } else {
            "已开启".to_string()
        };
        format!("IPv4: {}  |  IPv6: {}", v4, v6)
    }
}

#[derive(Debug, Clone)]
pub struct AdapterSummary {
    pub friendly_name: String,
    pub description: String,
    pub is_up: bool,
    pub ipv4_enabled: bool,
    pub dhcp_enabled: bool,
    pub ipv4: Option<String>,
    pub subnet_mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub ipv6_enabled: bool,
    pub ipv6: Option<String>,
    pub ipv6_prefix: Option<u8>,
    pub ipv6_gateway: Option<String>,
    pub ipv6_dns: Vec<String>,
}

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

pub fn list_adapters() -> Vec<AdapterSummary> {
    let mut size: u32 = 24 * 1024;
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

fn is_ipv6_link_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

fn extract_adapter_summary(adapter: &IP_ADAPTER_ADDRESSES_LH) -> AdapterSummary {
    let friendly_name = unsafe { pwstr_to_string(adapter.FriendlyName) };
    let description = unsafe { pwstr_to_string(adapter.Description) };
    let is_up = adapter.OperStatus == IfOperStatusUp;
    let flags = unsafe { adapter.Anonymous2.Flags };
    let ipv4_enabled = (flags & IP_ADAPTER_IPV4_ENABLED) != 0;
    let dhcp_enabled = (flags & IP_ADAPTER_DHCP_ENABLED) != 0;
    let ipv6_enabled = (flags & IP_ADAPTER_IPV6_ENABLED) != 0;

    let mut ipv4: Option<String> = None;
    let mut subnet_mask: Option<String> = None;
    let mut ipv6_global: Option<(String, u8)> = None;
    let mut ipv6_fallback: Option<(String, u8)> = None;

    let mut uni = adapter.FirstUnicastAddress;
    while !uni.is_null() {
        let u = unsafe { &*uni };
        if ipv4.is_none() {
            if let Some(ip) = sockaddr_to_ipv4(u.Address.lpSockaddr) {
                ipv4 = Some(ip.to_string());
                subnet_mask = Some(prefix_len_to_mask(u.OnLinkPrefixLength).to_string());
            }
        }
        if let Some(ip6) = sockaddr_to_ipv6(u.Address.lpSockaddr) {
            if !is_ipv6_link_local(ip6) && ipv6_global.is_none() {
                ipv6_global = Some((ip6.to_string(), u.OnLinkPrefixLength));
            } else if ipv6_fallback.is_none() {
                ipv6_fallback = Some((ip6.to_string(), u.OnLinkPrefixLength));
            }
        }
        uni = u.Next;
    }

    let (ipv6, ipv6_prefix) = match ipv6_global.or(ipv6_fallback) {
        Some((ip, prefix)) => (Some(ip), Some(prefix)),
        None => (None, None),
    };

    let mut gateway: Option<String> = None;
    let mut ipv6_gateway: Option<String> = None;
    let mut gw = adapter.FirstGatewayAddress;
    while !gw.is_null() {
        let g = unsafe { &*gw };
        if gateway.is_none() {
            if let Some(ip) = sockaddr_to_ipv4(g.Address.lpSockaddr) {
                gateway = Some(ip.to_string());
            }
        }
        if ipv6_gateway.is_none() {
            if let Some(ip6) = sockaddr_to_ipv6(g.Address.lpSockaddr) {
                ipv6_gateway = Some(ip6.to_string());
            }
        }
        gw = g.Next;
    }

    let mut dns: Vec<String> = Vec::new();
    let mut ipv6_dns: Vec<String> = Vec::new();
    let mut dns_ptr = adapter.FirstDnsServerAddress;
    while !dns_ptr.is_null() {
        let d = unsafe { &*dns_ptr };
        if let Some(ip) = sockaddr_to_ipv4(d.Address.lpSockaddr) {
            let s = ip.to_string();
            if !dns.contains(&s) {
                dns.push(s);
            }
        } else if let Some(ip6) = sockaddr_to_ipv6(d.Address.lpSockaddr) {
            // Skip Windows site-local default fec0:0:0:ffff::* placeholder DNS
            if ip6.segments()[0] != 0xfec0 {
                let s = ip6.to_string();
                if !ipv6_dns.contains(&s) {
                    ipv6_dns.push(s);
                }
            }
        }
        dns_ptr = d.Next;
    }

    AdapterSummary {
        friendly_name,
        description,
        is_up,
        ipv4_enabled,
        dhcp_enabled,
        ipv4,
        subnet_mask,
        gateway,
        dns,
        ipv6_enabled,
        ipv6,
        ipv6_prefix,
        ipv6_gateway,
        ipv6_dns,
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
            ipv4_enabled: false,
            dhcp_enabled: false,
            ipv4: None,
            subnet_mask: None,
            gateway: None,
            dns: Vec::new(),
            ipv6_enabled: false,
            ipv6: None,
            ipv6_prefix: None,
            ipv6_gateway: None,
            ipv6_dns: Vec::new(),
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
        ipv4_enabled: adapter.ipv4_enabled,
        dhcp_enabled: adapter.dhcp_enabled,
        ipv4: adapter.ipv4.clone(),
        subnet_mask: adapter.subnet_mask.clone(),
        gateway: adapter.gateway.clone(),
        dns: adapter.dns.clone(),
        ipv6_enabled: adapter.ipv6_enabled,
        ipv6: adapter.ipv6.clone(),
        ipv6_prefix: adapter.ipv6_prefix,
        ipv6_gateway: adapter.ipv6_gateway.clone(),
        ipv6_dns: adapter.ipv6_dns.clone(),
        mode,
        matched_profile_id: matched_profile.map(|p| p.id.clone()),
        matched_profile_name: matched_profile.map(|p| p.name.clone()),
    }
}

fn ipv4_matches_adapter(cfg: &Ipv4Config, adapter: &AdapterSummary) -> Option<u8> {
    match cfg {
        Ipv4Config::Disabled => {
            if !adapter.ipv4_enabled {
                Some(2)
            } else {
                None
            }
        }
        Ipv4Config::Dhcp { .. } => {
            if adapter.ipv4_enabled && adapter.dhcp_enabled {
                Some(2)
            } else {
                None
            }
        }
        Ipv4Config::Static {
            ip, gateway, dns, ..
        } => {
            if adapter.ipv4_enabled
                && !adapter.dhcp_enabled
                && adapter.ipv4.as_deref() == Some(ip.as_str())
                && adapter.gateway.as_deref() == Some(gateway.as_str())
            {
                if dns.is_empty() || adapter.dns == *dns {
                    Some(3)
                } else {
                    Some(2)
                }
            } else {
                None
            }
        }
    }
}

fn ipv6_matches_adapter(cfg: &Ipv6Config, adapter: &AdapterSummary) -> Option<u8> {
    match cfg {
        Ipv6Config::Disabled => {
            if !adapter.ipv6_enabled {
                Some(2)
            } else {
                None
            }
        }
        Ipv6Config::Dhcp => {
            if adapter.ipv6_enabled {
                Some(1)
            } else {
                None
            }
        }
        Ipv6Config::Static { ip, .. } => {
            if !adapter.ipv6_enabled {
                return None;
            }
            if let (Ok(cfg_ip), Some(cur_str)) = (ip.parse::<Ipv6Addr>(), adapter.ipv6.as_deref()) {
                if cur_str.parse::<Ipv6Addr>().ok() == Some(cfg_ip) {
                    return Some(3);
                }
            }
            None
        }
    }
}

fn match_adapter_profile<'a>(
    adapter: &AdapterSummary,
    profiles: &'a [NetworkProfile],
) -> (NetworkMode, Option<&'a NetworkProfile>) {
    if !adapter.is_up {
        return (NetworkMode::Disconnected, None);
    }

    let mut best: Option<(u8, &'a NetworkProfile)> = None;
    for p in profiles
        .iter()
        .filter(|p| p.interface_name.eq_ignore_ascii_case(&adapter.friendly_name))
    {
        if let (Some(s4), Some(s6)) = (
            ipv4_matches_adapter(&p.ipv4, adapter),
            ipv6_matches_adapter(&p.ipv6, adapter),
        ) {
            let score = s4 + s6;
            if best.map(|(b, _)| score > b).unwrap_or(true) {
                best = Some((score, p));
            }
        }
    }

    if let Some((_, p)) = best {
        let mode = if p.ipv4.is_dhcp() && !matches!(p.ipv6, Ipv6Config::Static { .. }) {
            NetworkMode::Dhcp
        } else {
            NetworkMode::Static
        };
        return (mode, Some(p));
    }

    if adapter.ipv4_enabled && adapter.dhcp_enabled {
        (NetworkMode::Dhcp, None)
    } else {
        (NetworkMode::StaticCustom, None)
    }
}

pub fn apply_profile(
    profile: &NetworkProfile,
    all_profiles: &[NetworkProfile],
) -> Result<NetworkStatus, String> {
    let iface = &profile.interface_name;

    // 1. Apply IPv4 configuration
    match &profile.ipv4 {
        Ipv4Config::Disabled => {
            set_adapter_binding(iface, "ms_tcpip", false)?;
        }
        Ipv4Config::Static {
            ip,
            subnet_mask,
            gateway,
            gateway_metric,
            dns,
        } => {
            let cur = query_interface_status(iface, all_profiles);
            if !cur.ipv4_enabled {
                set_adapter_binding(iface, "ms_tcpip", true)?;
                wait_for_binding(iface, true, false, all_profiles);
            }
            apply_ipv4_static(iface, ip, subnet_mask, gateway, *gateway_metric, dns)?;
        }
        Ipv4Config::Dhcp { renew_on_switch } => {
            let cur = query_interface_status(iface, all_profiles);
            if !cur.ipv4_enabled {
                set_adapter_binding(iface, "ms_tcpip", true)?;
                wait_for_binding(iface, true, false, all_profiles);
            }
            apply_ipv4_dhcp(iface, *renew_on_switch, all_profiles)?;
        }
    }

    // 2. Apply IPv6 configuration
    match &profile.ipv6 {
        Ipv6Config::Disabled => {
            set_adapter_binding(iface, "ms_tcpip6", false)?;
        }
        Ipv6Config::Dhcp => {
            apply_ipv6_dhcp(iface, all_profiles)?;
        }
        Ipv6Config::Static {
            ip,
            prefix_length,
            gateway,
            dns,
        } => {
            apply_ipv6_static(iface, ip, *prefix_length, gateway, dns, all_profiles)?;
        }
    }

    // Wait briefly for OS state to settle and return fresh status
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last = query_interface_status(iface, all_profiles);
    while Instant::now() < deadline {
        if last.matched_profile_id.as_deref() == Some(profile.id.as_str()) {
            return Ok(last);
        }
        std::thread::sleep(Duration::from_millis(300));
        last = query_interface_status(iface, all_profiles);
    }

    Ok(last)
}

fn wait_for_binding(
    iface: &str,
    want_v4: bool,
    want_v6: bool,
    all_profiles: &[NetworkProfile],
) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let st = query_interface_status(iface, all_profiles);
        if (!want_v4 || st.ipv4_enabled) && (!want_v6 || st.ipv6_enabled) {
            return;
        }
    }
}

fn set_adapter_binding(iface: &str, component_id: &str, enable: bool) -> Result<(), String> {
    let escaped = iface.replace('\'', "''");
    let cmdlet = if enable {
        "Enable-NetAdapterBinding"
    } else {
        "Disable-NetAdapterBinding"
    };
    let ps_cmd = format!(
        "{} -Name '{}' -ComponentID {} -Confirm:$false -ErrorAction Stop | Out-Null",
        cmdlet, escaped, component_id
    );
    let res = run_hidden_powershell(&ps_cmd)?;
    if !res.success {
        return Err(format!(
            "{} 网卡「{}」{} 失败 (code={:?}): {}",
            if enable { "开启" } else { "关闭" },
            iface,
            component_id,
            res.code,
            res.output
        ));
    }
    Ok(())
}

fn apply_ipv4_static(
    iface: &str,
    ip: &str,
    subnet_mask: &str,
    gateway: &str,
    gateway_metric: u32,
    dns: &[String],
) -> Result<(), String> {
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
            "设置静态 IPv4/网关失败 (code={:?}): {}",
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
                "设置静态 IPv4 主 DNS 失败 (code={:?}): {}",
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
                    "设置静态 IPv4 备用 DNS {} 失败 (code={:?}): {}",
                    dns_ip, add_out.code, add_out.output
                ));
            }
        }
    }

    Ok(())
}

fn apply_ipv4_dhcp(
    iface: &str,
    renew_on_switch: bool,
    all_profiles: &[NetworkProfile],
) -> Result<(), String> {
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

    let deadline = Instant::now() + Duration::from_secs(6);
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
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(350));
        last_status = query_interface_status(iface, all_profiles);
    }

    if last_status.dhcp_enabled {
        return Ok(());
    }

    Err(format!(
        "切换 IPv4 DHCP 模式失败。address(code={:?}): {}; dns(code={:?}): {}",
        addr_res.code, addr_res.output, dns_res.code, dns_res.output
    ))
}

fn apply_ipv6_dhcp(iface: &str, all_profiles: &[NetworkProfile]) -> Result<(), String> {
    let cur = query_interface_status(iface, all_profiles);
    if !cur.ipv6_enabled {
        set_adapter_binding(iface, "ms_tcpip6", true)?;
        wait_for_binding(iface, false, true, all_profiles);
    }

    let escaped = iface.replace('\'', "''");
    let ps_cmd = format!(
        "Get-NetIPAddress -InterfaceAlias '{0}' -AddressFamily IPv6 -PrefixOrigin Manual -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue; \
         Get-NetRoute -InterfaceAlias '{0}' -AddressFamily IPv6 -DestinationPrefix '::/0' -ErrorAction SilentlyContinue | Where-Object {{ $_.Protocol -eq 'NetMgmt' }} | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue; \
         Set-NetIPInterface -InterfaceAlias '{0}' -AddressFamily IPv6 -Dhcp Enabled -RouterDiscovery Enabled -ErrorAction Stop | Out-Null",
        escaped
    );
    let ps_res = run_hidden_powershell(&ps_cmd)?;
    if !ps_res.success {
        return Err(format!(
            "切换 IPv6 自动模式失败 (code={:?}): {}",
            ps_res.code, ps_res.output
        ));
    }

    let dns_args = vec![
        "interface".to_string(),
        "ipv6".to_string(),
        "set".to_string(),
        "dnsservers".to_string(),
        format!("name={}", iface),
        "source=dhcp".to_string(),
    ];
    let _ = run_hidden_command("netsh.exe", &dns_args);

    Ok(())
}

fn apply_ipv6_static(
    iface: &str,
    ip: &str,
    prefix_length: u8,
    gateway: &str,
    dns: &[String],
    all_profiles: &[NetworkProfile],
) -> Result<(), String> {
    let cur = query_interface_status(iface, all_profiles);
    if !cur.ipv6_enabled {
        set_adapter_binding(iface, "ms_tcpip6", true)?;
        wait_for_binding(iface, false, true, all_profiles);
    }

    let escaped = iface.replace('\'', "''");
    let gw_trimmed = gateway.trim();
    let new_ip_cmd = if gw_trimmed.is_empty() {
        format!(
            "New-NetIPAddress -InterfaceAlias '{0}' -AddressFamily IPv6 -IPAddress '{1}' -PrefixLength {2} -ErrorAction Stop | Out-Null",
            escaped, ip, prefix_length
        )
    } else {
        format!(
            "New-NetIPAddress -InterfaceAlias '{0}' -AddressFamily IPv6 -IPAddress '{1}' -PrefixLength {2} -DefaultGateway '{3}' -ErrorAction Stop | Out-Null",
            escaped, ip, prefix_length, gw_trimmed
        )
    };

    let ps_cmd = format!(
        "Get-NetIPAddress -InterfaceAlias '{0}' -AddressFamily IPv6 -PrefixOrigin Manual -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue; \
         Get-NetRoute -InterfaceAlias '{0}' -AddressFamily IPv6 -DestinationPrefix '::/0' -ErrorAction SilentlyContinue | Where-Object {{ $_.Protocol -eq 'NetMgmt' }} | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue; \
         Set-NetIPInterface -InterfaceAlias '{0}' -AddressFamily IPv6 -Dhcp Disabled -RouterDiscovery Disabled -ErrorAction SilentlyContinue | Out-Null; \
         {1}",
        escaped, new_ip_cmd
    );
    let ps_res = run_hidden_powershell(&ps_cmd)?;
    if !ps_res.success {
        return Err(format!(
            "设置静态 IPv6 失败 (code={:?}): {}",
            ps_res.code, ps_res.output
        ));
    }

    if dns.is_empty() {
        let dns_args = vec![
            "interface".to_string(),
            "ipv6".to_string(),
            "set".to_string(),
            "dnsservers".to_string(),
            format!("name={}", iface),
            "source=dhcp".to_string(),
        ];
        let _ = run_hidden_command("netsh.exe", &dns_args);
    } else {
        let primary_args = vec![
            "interface".to_string(),
            "ipv6".to_string(),
            "set".to_string(),
            "dnsservers".to_string(),
            format!("name={}", iface),
            "source=static".to_string(),
            format!("address={}", dns[0]),
            "register=primary".to_string(),
            "validate=no".to_string(),
        ];
        let dns_out = run_hidden_command("netsh.exe", &primary_args)?;
        if !dns_out.success {
            return Err(format!(
                "设置静态 IPv6 主 DNS 失败 (code={:?}): {}",
                dns_out.code, dns_out.output
            ));
        }

        for (idx, dns_ip) in dns.iter().enumerate().skip(1) {
            let add_args = vec![
                "interface".to_string(),
                "ipv6".to_string(),
                "add".to_string(),
                "dnsservers".to_string(),
                format!("name={}", iface),
                format!("address={}", dns_ip),
                format!("index={}", idx + 1),
                "validate=no".to_string(),
            ];
            let add_out = run_hidden_command("netsh.exe", &add_args)?;
            if !add_out.success {
                return Err(format!(
                    "设置静态 IPv6 备用 DNS {} 失败 (code={:?}): {}",
                    dns_ip, add_out.code, add_out.output
                ));
            }
        }
    }

    Ok(())
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

fn sockaddr_to_ipv6(sockaddr: *const SOCKADDR) -> Option<Ipv6Addr> {
    if sockaddr.is_null() {
        return None;
    }
    unsafe {
        if (*sockaddr).sa_family != AF_INET6 {
            return None;
        }
        let sin6 = &*(sockaddr.cast::<SOCKADDR_IN6>());
        let bytes = sin6.sin6_addr.u.Byte;
        Some(Ipv6Addr::from(bytes))
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

fn run_hidden_powershell(ps_cmd: &str) -> Result<CmdOutput, String> {
    let args = vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-WindowStyle".to_string(),
        "Hidden".to_string(),
        "-Command".to_string(),
        ps_cmd.to_string(),
    ];
    run_hidden_command("powershell.exe", &args)
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
                AF_UNSPEC,
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
    fn matches_adapter_dual_stack_profile() {
        let profiles = vec![
            NetworkProfile {
                id: "p1".to_string(),
                name: "静态 A".to_string(),
                interface_name: "WLAN".to_string(),
                ipv4: Ipv4Config::Static {
                    ip: "192.168.1.10".to_string(),
                    subnet_mask: "255.255.255.0".to_string(),
                    gateway: "192.168.1.2".to_string(),
                    gateway_metric: 0,
                    dns: vec!["192.168.1.1".to_string()],
                },
                ipv6: Ipv6Config::Disabled,
            },
            NetworkProfile {
                id: "p2".to_string(),
                name: "DHCP 双栈".to_string(),
                interface_name: "WLAN".to_string(),
                ipv4: Ipv4Config::Dhcp {
                    renew_on_switch: true,
                },
                ipv6: Ipv6Config::Dhcp,
            },
        ];

        let adapter = AdapterSummary {
            friendly_name: "WLAN".to_string(),
            description: "Wi-Fi".to_string(),
            is_up: true,
            ipv4_enabled: true,
            dhcp_enabled: false,
            ipv4: Some("192.168.1.10".to_string()),
            subnet_mask: Some("255.255.255.0".to_string()),
            gateway: Some("192.168.1.2".to_string()),
            dns: vec!["192.168.1.1".to_string()],
            ipv6_enabled: false,
            ipv6: None,
            ipv6_prefix: None,
            ipv6_gateway: None,
            ipv6_dns: Vec::new(),
        };

        let (mode, matched) = match_adapter_profile(&adapter, &profiles);
        assert_eq!(mode, NetworkMode::Static);
        assert_eq!(matched.unwrap().id, "p1");
    }
}
