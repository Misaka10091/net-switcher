use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::net::Ipv4Addr;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ipv6Setting {
    #[default]
    Keep,
    Enable,
    Disable,
}

impl Ipv6Setting {
    pub fn display_name(self) -> &'static str {
        match self {
            Ipv6Setting::Keep => "保持不变",
            Ipv6Setting::Enable => "开启 IPv6",
            Ipv6Setting::Disable => "关闭 IPv6",
        }
    }

    pub fn short_badge(self) -> Option<&'static str> {
        match self {
            Ipv6Setting::Keep => None,
            Ipv6Setting::Enable => Some("开IPv6"),
            Ipv6Setting::Disable => Some("关IPv6"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProfile {
    pub id: String,
    pub name: String,
    pub interface_name: String,
    pub mode: ProfileMode,
    #[serde(default)]
    pub ipv6: Ipv6Setting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProfileMode {
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
}

impl ProfileMode {
    pub fn is_dhcp(&self) -> bool {
        matches!(self, ProfileMode::Dhcp { .. })
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            ProfileMode::Dhcp { .. } => "DHCP",
            ProfileMode::Static { .. } => "静态 IP",
        }
    }

    pub fn summary(&self) -> String {
        match self {
            ProfileMode::Dhcp { renew_on_switch } => {
                if *renew_on_switch {
                    "自动获取 IP/网关/DNS (切后刷新租约)".to_string()
                } else {
                    "自动获取 IP/网关/DNS".to_string()
                }
            }
            ProfileMode::Static { ip, gateway, .. } => format!("{} → 网关 {}", ip, gateway),
        }
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
    2
}

fn default_true() -> bool {
    true
}

fn default_refresh_interval() -> u32 {
    15
}

// Legacy v1 config format for seamless automatic migration
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyConfig {
    interface_name: String,
    #[serde(rename = "static")]
    static_cfg: LegacyStaticConfig,
    #[serde(default)]
    dhcp: LegacyDhcpConfig,
    #[serde(default)]
    ui: UiConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyStaticConfig {
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
struct LegacyDhcpConfig {
    #[serde(default = "default_true")]
    renew_on_switch: bool,
}

impl Default for LegacyDhcpConfig {
    fn default() -> Self {
        Self {
            renew_on_switch: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfigFile {
    V2(Config),
    Legacy(LegacyConfig),
}

impl Config {
    pub fn default_for_interface(interface_name: String) -> Self {
        Self {
            version: 2,
            active_interface: interface_name.clone(),
            profiles: vec![
                NetworkProfile {
                    id: "default-static".to_string(),
                    name: "固定网关".to_string(),
                    interface_name: interface_name.clone(),
                    mode: ProfileMode::Static {
                        ip: "192.168.1.100".to_string(),
                        subnet_mask: "255.255.255.0".to_string(),
                        gateway: "192.168.1.2".to_string(),
                        gateway_metric: 0,
                        dns: vec!["192.168.1.1".to_string()],
                    },
                    ipv6: Ipv6Setting::Keep,
                },
                NetworkProfile {
                    id: "default-dhcp".to_string(),
                    name: "DHCP 自动获取".to_string(),
                    interface_name,
                    mode: ProfileMode::Dhcp {
                        renew_on_switch: true,
                    },
                    ipv6: Ipv6Setting::Keep,
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
        ConfigFile::V2(mut c) => {
            c.version = 2;
            c
        }
        ConfigFile::Legacy(legacy) => Config {
            version: 2,
            active_interface: legacy.interface_name.clone(),
            profiles: vec![
                NetworkProfile {
                    id: "migrated-static".to_string(),
                    name: "固定网关".to_string(),
                    interface_name: legacy.interface_name.clone(),
                    mode: ProfileMode::Static {
                        ip: legacy.static_cfg.ip,
                        subnet_mask: legacy.static_cfg.subnet_mask,
                        gateway: legacy.static_cfg.gateway,
                        gateway_metric: legacy.static_cfg.gateway_metric,
                        dns: legacy.static_cfg.dns,
                    },
                    ipv6: Ipv6Setting::Keep,
                },
                NetworkProfile {
                    id: "migrated-dhcp".to_string(),
                    name: "DHCP 自动获取".to_string(),
                    interface_name: legacy.interface_name,
                    mode: ProfileMode::Dhcp {
                        renew_on_switch: legacy.dhcp.renew_on_switch,
                    },
                    ipv6: Ipv6Setting::Keep,
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

    if let ProfileMode::Static {
        ip,
        subnet_mask,
        gateway,
        dns,
        ..
    } = &profile.mode
    {
        ip.parse::<Ipv4Addr>()
            .map_err(|_| format!("Profile「{}」的静态 IP 无效: {}", profile.name, ip))?;
        subnet_mask.parse::<Ipv4Addr>().map_err(|_| {
            format!(
                "Profile「{}」的子网掩码无效: {}",
                profile.name, subnet_mask
            )
        })?;
        gateway
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("Profile「{}」的默认网关无效: {}", profile.name, gateway))?;
        for d in dns {
            d.parse::<Ipv4Addr>()
                .map_err(|_| format!("Profile「{}」包含无效的 DNS 地址: {}", profile.name, d))?;
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
          "dhcp": { "renewOnSwitch": true },
          "ui": {
            "showNotifications": true,
            "refreshIntervalSeconds": 15,
            "leftClickToggle": true
          }
        }"#;
        let cfg = parse_config_str(json).expect("legacy config should migrate");
        assert_eq!(cfg.version, 2);
        assert_eq!(cfg.active_interface, "WLAN");
        assert_eq!(cfg.profiles.len(), 2);
        assert!(!cfg.profiles[0].mode.is_dhcp());
        assert!(cfg.profiles[1].mode.is_dhcp());
    }

    #[test]
    fn parses_v2_multi_profile_config() {
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
                "gatewayMetric": 0,
                "dns": ["192.168.1.1", "223.5.5.5"]
              }
            },
            {
              "id": "home-dhcp",
              "name": "自动 DHCP",
              "interfaceName": "WLAN",
              "ipv6": "enable",
              "mode": {
                "type": "dhcp",
                "renewOnSwitch": true
              }
            },
            {
              "id": "office-eth",
              "name": "办公室有线",
              "interfaceName": "Ethernet",
              "mode": {
                "type": "static",
                "ip": "10.0.0.50",
                "subnetMask": "255.255.255.0",
                "gateway": "10.0.0.1",
                "dns": ["10.0.0.1"]
              }
            }
          ]
        }"#;
        let cfg = parse_config_str(json).expect("v2 config should parse");
        assert_eq!(cfg.active_profiles().len(), 2);
        assert_eq!(cfg.profiles_for_interface("Ethernet").len(), 1);
        assert_eq!(cfg.profiles[0].ipv6, Ipv6Setting::Disable);
        assert_eq!(cfg.profiles[1].ipv6, Ipv6Setting::Enable);
        assert_eq!(cfg.profiles[2].ipv6, Ipv6Setting::Keep);
    }
}
