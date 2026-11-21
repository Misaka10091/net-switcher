use serde::{Deserialize, Serialize};
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub interface_name: String,
    #[serde(rename = "static")]
    pub static_cfg: StaticConfig,
    #[serde(default)]
    pub dhcp: DhcpConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticConfig {
    pub ip: String,
    pub subnet_mask: String,
    pub gateway: String,
    #[serde(default)]
    pub gateway_metric: u32,
    #[serde(default)]
    pub dns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DhcpConfig {
    #[serde(default = "default_true")]
    pub renew_on_switch: bool,
}

impl Default for DhcpConfig {
    fn default() -> Self {
        Self {
            renew_on_switch: true,
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

fn default_true() -> bool {
    true
}

fn default_refresh_interval() -> u32 {
    15
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

pub fn load_config(path: &Path) -> Result<Config, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("无法读取配置文件 {}: {}", path.display(), e))?;
    let trimmed = raw.trim_start_matches('\u{feff}');
    let cfg: Config = serde_json::from_str(trimmed)
        .map_err(|e| format!("解析配置文件 {} 失败: {}", path.display(), e))?;
    validate_config(&cfg)?;
    Ok(cfg)
}

pub fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    validate_config(cfg)?;
    let content = serde_json::to_string_pretty(cfg)
        .map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(path, format!("{}\n", content))
        .map_err(|e| format!("写入配置文件 {} 失败: {}", path.display(), e))?;
    Ok(())
}

pub fn validate_config(cfg: &Config) -> Result<(), String> {
    if cfg.interface_name.trim().is_empty() {
        return Err("配置项 interfaceName 不能为空".to_string());
    }
    cfg.static_cfg
        .ip
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("static.ip 不是有效的 IPv4 地址: {}", cfg.static_cfg.ip))?;
    cfg.static_cfg
        .subnet_mask
        .parse::<Ipv4Addr>()
        .map_err(|_| {
            format!(
                "static.subnetMask 不是有效的 IPv4 掩码: {}",
                cfg.static_cfg.subnet_mask
            )
        })?;
    cfg.static_cfg
        .gateway
        .parse::<Ipv4Addr>()
        .map_err(|_| {
            format!(
                "static.gateway 不是有效的 IPv4 地址: {}",
                cfg.static_cfg.gateway
            )
        })?;
    for dns in &cfg.static_cfg.dns {
        dns.parse::<Ipv4Addr>()
            .map_err(|_| format!("static.dns 包含无效的 IPv4 地址: {}", dns))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_config_format() {
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
        let cfg: Config = serde_json::from_str(json).expect("valid config");
        assert!(validate_config(&cfg).is_ok());
        assert_eq!(cfg.interface_name, "WLAN");
        assert_eq!(cfg.static_cfg.gateway, "192.168.1.2");
        assert_eq!(cfg.static_cfg.dns, vec!["192.168.1.1"]);
    }
}
