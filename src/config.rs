use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    pub active_interface: String,
    #[serde(default)]
    pub profiles: Vec<NetworkProfile>,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProfile {
    pub id: String,
    pub name: String,
    pub interface_name: String,
    pub ipv4: Ipv4Config,
    #[serde(default)]
    pub ipv6: Ipv6Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Ipv4Config {
    Dhcp {
        #[serde(default = "default_true", rename = "renewOnSwitch")]
        renew_on_switch: bool,
    },
    Static {
        ip: String,
        #[serde(rename = "subnetMask")]
        subnet_mask: String,
        gateway: String,
        #[serde(default, rename = "gatewayMetric")]
        gateway_metric: u32,
        #[serde(default)]
        dns: Vec<String>,
    },
    Disabled,
}

impl Ipv4Config {
    pub fn short_label(&self) -> &'static str {
        match self {
            Ipv4Config::Dhcp { .. } => "v4:DHCP",
            Ipv4Config::Static { .. } => "v4:静态",
            Ipv4Config::Disabled => "v4:关",
        }
    }

    pub fn is_dhcp(&self) -> bool {
        matches!(self, Ipv4Config::Dhcp { .. })
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Ipv4Config::Disabled)
    }

    pub fn summary(&self) -> String {
        match self {
            Ipv4Config::Dhcp { renew_on_switch } => {
                if *renew_on_switch {
                    "IPv4: DHCP(刷新租约)".to_string()
                } else {
                    "IPv4: DHCP".to_string()
                }
            }
            Ipv4Config::Static { ip, gateway, .. } => format!("IPv4: {} → {}", ip, gateway),
            Ipv4Config::Disabled => "IPv4: 关闭".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Ipv6Config {
    #[default]
    Dhcp,
    Static {
        ip: String,
        #[serde(default = "default_ipv6_prefix", rename = "prefixLength")]
        prefix_length: u8,
        #[serde(default)]
        gateway: String,
        #[serde(default)]
        dns: Vec<String>,
    },
    Disabled,
}

impl Ipv6Config {
    pub fn short_label(&self) -> &'static str {
        match self {
            Ipv6Config::Dhcp => "v6:DHCP",
            Ipv6Config::Static { .. } => "v6:静态",
            Ipv6Config::Disabled => "v6:关",
        }
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Ipv6Config::Disabled)
    }

    pub fn summary(&self) -> String {
        match self {
            Ipv6Config::Dhcp => "IPv6: 自动".to_string(),
            Ipv6Config::Static {
                ip, prefix_length, ..
            } => format!("IPv6: {}/{}", ip, prefix_length),
            Ipv6Config::Disabled => "IPv6: 关闭".to_string(),
        }
    }
}

impl NetworkProfile {
    pub fn short_mode_summary(&self) -> String {
        format!("{} / {}", self.ipv4.short_label(), self.ipv6.short_label())
    }

    pub fn detail_summary(&self) -> String {
        format!("{}  |  {}", self.ipv4.summary(), self.ipv6.summary())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    #[serde(default = "default_true")]
    pub show_notifications: bool,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_seconds: u32,
    #[serde(default = "default_true")]
    pub left_click_toggle: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_notifications: true,
            refresh_interval_seconds: 15,
            left_click_toggle: true,
        }
    }
}

fn default_version() -> u32 {
    3
}

fn default_true() -> bool {
    true
}

fn default_refresh_interval() -> u32 {
    15
}

fn default_ipv6_prefix() -> u8 {
    64
}

// Legacy v2 config format for seamless automatic migration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV2Config {
    active_interface: String,
    #[serde(default)]
    profiles: Vec<LegacyV2Profile>,
    #[serde(default)]
    ui: UiConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV2Profile {
    id: String,
    name: String,
    interface_name: String,
    mode: Ipv4Config,
    #[serde(default)]
    ipv6: Option<String>,
}

// Legacy v1 config format for seamless automatic migration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV1Config {
    interface_name: String,
    #[serde(rename = "static")]
    static_cfg: LegacyV1StaticConfig,
    #[serde(default)]
    dhcp: LegacyV1DhcpConfig,
    #[serde(default)]
    ui: UiConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV1StaticConfig {
    ip: String,
    subnet_mask: String,
    gateway: String,
    #[serde(default)]
    gateway_metric: u32,
    #[serde(default)]
    dns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyV1DhcpConfig {
    #[serde(default = "default_true")]
    renew_on_switch: bool,
}

impl Default for LegacyV1DhcpConfig {
    fn default() -> Self {
        Self {
            renew_on_switch: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigFile {
    V3(Config),
    V2(LegacyV2Config),
    V1(LegacyV1Config),
}

impl Config {
    pub fn default_for_interface(interface_name: String) -> Self {
        Self {
            version: 3,
            active_interface: interface_name.clone(),
            profiles: vec![
                NetworkProfile {
                    id: "default-static".to_string(),
                    name: "固定网关".to_string(),
                    interface_name: interface_name.clone(),
                    ipv4: Ipv4Config::Static {
                        ip: "192.168.1.100".to_string(),
                        subnet_mask: "255.255.255.0".to_string(),
                        gateway: "192.168.1.2".to_string(),
                        gateway_metric: 0,
                        dns: vec!["192.168.1.1".to_string()],
                    },
                    ipv6: Ipv6Config::Disabled,
                },
                NetworkProfile {
                    id: "default-dhcp".to_string(),
                    name: "DHCP 自动获取".to_string(),
                    interface_name,
                    ipv4: Ipv4Config::Dhcp {
                        renew_on_switch: true,
                    },
                    ipv6: Ipv6Config::Dhcp,
                },
            ],
            ui: UiConfig::default(),
        }
    }

    pub fn profiles_for_interface(&self, iface: &str) -> Vec<&NetworkProfile> {
        self.profiles
            .iter()
            .filter(|p| p.interface_name.eq_ignore_ascii_case(iface))
            .collect()
    }

    pub fn active_profiles(&self) -> Vec<&NetworkProfile> {
        self.profiles_for_interface(&self.active_interface)
    }

    pub fn next_profile_id(&self, prefix: &str) -> String {
        let existing: HashSet<&str> = self.profiles.iter().map(|p| p.id.as_str()).collect();
        for n in 1..10_000 {
            let candidate = format!("{}-{}", prefix, n);
            if !existing.contains(candidate.as_str()) {
                return candidate;
            }
        }
        format!("{}-extra", prefix)
    }
}

pub fn resolve_config_path() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let candidate = exe_dir.join("config.json");
            if candidate.exists() {
                return candidate;
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("config.json");
        if candidate.exists() {
            return candidate;
        }
    }

    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.json")))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

pub fn parse_config_str(raw: &str) -> Result<Config, String> {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let parsed: ConfigFile =
        serde_json::from_str(trimmed).map_err(|e| format!("JSON 格式解析失败: {}", e))?;

    let cfg = match parsed {
        ConfigFile::V3(mut c) => {
            c.version = 3;
            c
        }
        ConfigFile::V2(v2) => Config {
            version: 3,
            active_interface: v2.active_interface,
            profiles: v2
                .profiles
                .into_iter()
                .map(|p| {
                    let ipv6 = match p.ipv6.as_deref() {
                        Some("disable") => Ipv6Config::Disabled,
                        _ => Ipv6Config::Dhcp,
                    };
                    NetworkProfile {
                        id: p.id,
                        name: p.name,
                        interface_name: p.interface_name,
                        ipv4: p.mode,
                        ipv6,
                    }
                })
                .collect(),
            ui: v2.ui,
        },
        ConfigFile::V1(legacy) => Config {
            version: 3,
            active_interface: legacy.interface_name.clone(),
            profiles: vec![
                NetworkProfile {
                    id: "migrated-static".to_string(),
                    name: "固定网关".to_string(),
                    interface_name: legacy.interface_name.clone(),
                    ipv4: Ipv4Config::Static {
                        ip: legacy.static_cfg.ip,
                        subnet_mask: legacy.static_cfg.subnet_mask,
                        gateway: legacy.static_cfg.gateway,
                        gateway_metric: legacy.static_cfg.gateway_metric,
                        dns: legacy.static_cfg.dns,
                    },
                    ipv6: Ipv6Config::Dhcp,
                },
                NetworkProfile {
                    id: "migrated-dhcp".to_string(),
                    name: "DHCP 自动获取".to_string(),
                    interface_name: legacy.interface_name,
                    ipv4: Ipv4Config::Dhcp {
                        renew_on_switch: legacy.dhcp.renew_on_switch,
                    },
                    ipv6: Ipv6Config::Dhcp,
                },
            ],
            ui: legacy.ui,
        },
    };

    validate_config(&cfg)?;
    Ok(cfg)
}

pub fn load_config(path: &Path) -> Result<Config, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("无法读取配置文件 {}: {}", path.display(), e))?;
    parse_config_str(&raw).map_err(|e| format!("配置文件 {} 无效: {}", path.display(), e))
}

pub fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    validate_config(cfg)?;
    let content = serde_json::to_string_pretty(cfg)
        .map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(path, format!("{}\n", content))
        .map_err(|e| format!("写入配置文件 {} 失败: {}", path.display(), e))?;
    Ok(())
}

pub fn validate_profile(profile: &NetworkProfile) -> Result<(), String> {
    if profile.id.trim().is_empty() {
        return Err("Profile ID 不能为空".to_string());
    }
    if profile.name.trim().is_empty() {
        return Err("Profile 名称不能为空".to_string());
    }
    if profile.interface_name.trim().is_empty() {
        return Err(format!("Profile「{}」未绑定目标网卡", profile.name));
    }

    if let Ipv4Config::Static {
        ip,
        subnet_mask,
        gateway,
        dns,
        ..
    } = &profile.ipv4
    {
        ip.parse::<Ipv4Addr>()
            .map_err(|_| format!("Profile「{}」的静态 IPv4 无效: {}", profile.name, ip))?;
        subnet_mask.parse::<Ipv4Addr>().map_err(|_| {
            format!(
                "Profile「{}」的子网掩码无效: {}",
                profile.name, subnet_mask
            )
        })?;
        gateway
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("Profile「{}」的 IPv4 网关无效: {}", profile.name, gateway))?;
        for d in dns {
            d.parse::<Ipv4Addr>().map_err(|_| {
                format!("Profile「{}」包含无效的 IPv4 DNS: {}", profile.name, d)
            })?;
        }
    }

    if let Ipv6Config::Static {
        ip,
        prefix_length,
        gateway,
        dns,
    } = &profile.ipv6
    {
        ip.parse::<Ipv6Addr>()
            .map_err(|_| format!("Profile「{}」的静态 IPv6 无效: {}", profile.name, ip))?;
        if *prefix_length == 0 || *prefix_length > 128 {
            return Err(format!(
                "Profile「{}」的 IPv6 前缀长度需在 1~128 之间: {}",
                profile.name, prefix_length
            ));
        }
        if !gateway.trim().is_empty() {
            gateway.trim().parse::<Ipv6Addr>().map_err(|_| {
                format!("Profile「{}」的 IPv6 网关无效: {}", profile.name, gateway)
            })?;
        }
        for d in dns {
            d.parse::<Ipv6Addr>().map_err(|_| {
                format!("Profile「{}」包含无效的 IPv6 DNS: {}", profile.name, d)
            })?;
        }
    }

    Ok(())
}

pub fn validate_config(cfg: &Config) -> Result<(), String> {
    if cfg.active_interface.trim().is_empty() {
        return Err("activeInterface（当前目标网卡）不能为空".to_string());
    }
    let mut ids = HashSet::new();
    for profile in &cfg.profiles {
        validate_profile(profile)?;
        if !ids.insert(profile.id.as_str()) {
            return Err(format!("存在重复的 Profile ID: {}", profile.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_v1_config() {
        let json = r#"{
          "interfaceName": "WLAN",
          "static": {
            "ip": "192.168.1.100",
            "subnetMask": "255.255.255.0",
            "gateway": "192.168.1.2",
            "gatewayMetric": 0,
            "dns": ["192.168.1.1"]
          },
          "dhcp": { "renewOnSwitch": true }
        }"#;
        let cfg = parse_config_str(json).expect("v1 config should migrate");
        assert_eq!(cfg.version, 3);
        assert_eq!(cfg.active_interface, "WLAN");
        assert_eq!(cfg.profiles.len(), 2);
        assert!(!cfg.profiles[0].ipv4.is_dhcp());
        assert!(cfg.profiles[1].ipv4.is_dhcp());
    }

    #[test]
    fn migrates_legacy_v2_config() {
        let json = r#"{
          "version": 2,
          "activeInterface": "WLAN",
          "profiles": [
            {
              "id": "home-gw",
              "name": "家庭旁路由",
              "interfaceName": "WLAN",
              "ipv6": "disable",
              "mode": {
                "type": "static",
                "ip": "192.168.1.100",
                "subnetMask": "255.255.255.0",
                "gateway": "192.168.1.2",
                "dns": ["192.168.1.1"]
              }
            },
            {
              "id": "home-dhcp",
              "name": "自动 DHCP",
              "interfaceName": "WLAN",
              "ipv6": "enable",
              "mode": { "type": "dhcp", "renewOnSwitch": true }
            }
          ]
        }"#;
        let cfg = parse_config_str(json).expect("v2 config should migrate");
        assert_eq!(cfg.version, 3);
        assert!(cfg.profiles[0].ipv6.is_disabled());
        assert!(matches!(cfg.profiles[1].ipv6, Ipv6Config::Dhcp));
    }

    #[test]
    fn parses_v3_independent_v4_v6_config() {
        let json = r#"{
          "version": 3,
          "activeInterface": "WLAN",
          "profiles": [
            {
              "id": "dual-static",
              "name": "双栈静态",
              "interfaceName": "WLAN",
              "ipv4": {
                "type": "static",
                "ip": "192.168.1.100",
                "subnetMask": "255.255.255.0",
                "gateway": "192.168.1.2",
                "dns": ["192.168.1.1"]
              },
              "ipv6": {
                "type": "static",
                "ip": "fd00::100",
                "prefixLength": 64,
                "gateway": "fd00::1",
                "dns": ["2400:3200::1"]
              }
            },
            {
              "id": "v6-only",
              "name": "纯 IPv6",
              "interfaceName": "WLAN",
              "ipv4": { "type": "disabled" },
              "ipv6": { "type": "dhcp" }
            }
          ]
        }"#;
        let cfg = parse_config_str(json).expect("v3 config should parse");
        assert_eq!(cfg.profiles.len(), 2);
        assert!(matches!(cfg.profiles[0].ipv6, Ipv6Config::Static { .. }));
        assert!(cfg.profiles[1].ipv4.is_disabled());
    }
}
