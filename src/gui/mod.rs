mod text_input;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Receiver};

use gpui::{
    AnyElement, App, Application, AsyncApp, Bounds, Context, Entity, FocusHandle, Focusable,
    KeyBinding, MouseButton, SharedString, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};

use crate::config::{
    Config, Ipv4Config, Ipv6Config, NetworkProfile, load_config, save_config, validate_config,
    validate_profile,
};
use crate::load_or_default_config;
use crate::net::{
    AdapterSummary, NetworkMode, NetworkStatus, apply_profile, list_adapters,
    query_interface_status,
};
use crate::tray::{GuiCommand, TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG};
use crate::winutil::{focus_window_by_title, is_elevated, notify_tray_message};

use text_input::{TextInput, bind_text_input_keys};

pub const GUI_WINDOW_TITLE: &str = "Net Switcher — 网卡与 Profile 配置";

actions!(net_gui, [CloseWindow]);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ipv4Kind {
    Static,
    Dhcp,
    Disabled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ipv6Kind {
    Dhcp,
    Static,
    Disabled,
}

#[derive(Clone)]
enum Banner {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
}

struct EditorFields {
    name: String,
    v4_kind: Ipv4Kind,
    v4_renew: bool,
    v4_ip: String,
    v4_mask: String,
    v4_gateway: String,
    v4_metric: String,
    v4_dns: String,
    v6_kind: Ipv6Kind,
    v6_ip: String,
    v6_prefix: String,
    v6_gateway: String,
    v6_dns: String,
}

struct ConfigWindow {
    focus_handle: FocusHandle,
    config_path: PathBuf,
    saved_config: Config,
    draft_config: Config,
    selected_profile_id: Option<String>,
    adapters: Vec<AdapterSummary>,
    current_status: NetworkStatus,
    is_admin: bool,
    switching_profile_name: Option<String>,
    banner: Option<Banner>,

    // Profile editor state
    name_input: Entity<TextInput>,
    v4_kind: Ipv4Kind,
    v4_renew: bool,
    v4_ip_input: Entity<TextInput>,
    v4_mask_input: Entity<TextInput>,
    v4_gateway_input: Entity<TextInput>,
    v4_metric_input: Entity<TextInput>,
    v4_dns_input: Entity<TextInput>,

    v6_kind: Ipv6Kind,
    v6_ip_input: Entity<TextInput>,
    v6_prefix_input: Entity<TextInput>,
    v6_gateway_input: Entity<TextInput>,
    v6_dns_input: Entity<TextInput>,

    // Global UI settings
    interval_input: Entity<TextInput>,
    show_notifications: bool,
}

impl Focusable for ConfigWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ConfigWindow {
    fn new(cx: &mut Context<Self>, config: Config, config_path: PathBuf) -> Self {
        let adapters = list_adapters();
        let current_status =
            query_interface_status(&config.active_interface, &config.profiles);

        let initial_profile_id = current_status
            .matched_profile_id
            .clone()
            .or_else(|| config.active_profiles().first().map(|p| p.id.clone()));

        let initial_profile = initial_profile_id
            .as_deref()
            .and_then(|id| config.profiles.iter().find(|p| p.id == id))
            .cloned();

        let fields = extract_editor_fields(initial_profile.as_ref());

        let name_input = cx.new(|cx| TextInput::new(cx, fields.name, "如 旁路由固定网关 / DHCP"));
        let v4_ip_input = cx.new(|cx| TextInput::new(cx, fields.v4_ip, "如 192.168.1.100"));
        let v4_mask_input = cx.new(|cx| TextInput::new(cx, fields.v4_mask, "如 255.255.255.0"));
        let v4_gateway_input = cx.new(|cx| TextInput::new(cx, fields.v4_gateway, "如 192.168.1.2"));
        let v4_metric_input = cx.new(|cx| TextInput::new(cx, fields.v4_metric, "0 表示自动跃点"));
        let v4_dns_input =
            cx.new(|cx| TextInput::new(cx, fields.v4_dns, "如 192.168.1.1, 223.5.5.5"));

        let v6_ip_input = cx.new(|cx| TextInput::new(cx, fields.v6_ip, "如 fd00::100"));
        let v6_prefix_input = cx.new(|cx| TextInput::new(cx, fields.v6_prefix, "默认 64"));
        let v6_gateway_input =
            cx.new(|cx| TextInput::new(cx, fields.v6_gateway, "可选，如 fd00::1"));
        let v6_dns_input =
            cx.new(|cx| TextInput::new(cx, fields.v6_dns, "可选，如 2400:3200::1"));

        let interval_input = cx.new(|cx| {
            TextInput::new(cx, config.ui.refresh_interval_seconds.to_string(), "默认 15")
        });

        let show_notifications = config.ui.show_notifications;

        Self {
            focus_handle: cx.focus_handle(),
            config_path,
            saved_config: config.clone(),
            draft_config: config,
            selected_profile_id: initial_profile_id,
            adapters,
            current_status,
            is_admin: is_elevated(),
            switching_profile_name: None,
            banner: None,
            name_input,
            v4_kind: fields.v4_kind,
            v4_renew: fields.v4_renew,
            v4_ip_input,
            v4_mask_input,
            v4_gateway_input,
            v4_metric_input,
            v4_dns_input,
            v6_kind: fields.v6_kind,
            v6_ip_input,
            v6_prefix_input,
            v6_gateway_input,
            v6_dns_input,
            interval_input,
            show_notifications,
        }
    }

    fn refresh_live_only(&mut self, cx: &mut Context<Self>) {
        self.adapters = list_adapters();
        self.current_status = query_interface_status(
            &self.draft_config.active_interface,
            &self.draft_config.profiles,
        );
        cx.notify();
    }

    fn sync_editor_into_draft(&mut self, cx: &App) -> Result<(), String> {
        let interval_str = self.interval_input.read(cx).text().trim().to_string();
        if let Ok(v) = interval_str.parse::<u32>() {
            self.draft_config.ui.refresh_interval_seconds = v.max(3);
        }
        self.draft_config.ui.show_notifications = self.show_notifications;

        let Some(selected_id) = self.selected_profile_id.clone() else {
            return Ok(());
        };
        let Some(profile) = self
            .draft_config
            .profiles
            .iter_mut()
            .find(|p| p.id == selected_id)
        else {
            return Ok(());
        };

        profile.name = self.name_input.read(cx).text().trim().to_string();

        profile.ipv4 = match self.v4_kind {
            Ipv4Kind::Dhcp => Ipv4Config::Dhcp {
                renew_on_switch: self.v4_renew,
            },
            Ipv4Kind::Disabled => Ipv4Config::Disabled,
            Ipv4Kind::Static => {
                let ip = self.v4_ip_input.read(cx).text().trim().to_string();
                let subnet_mask = self.v4_mask_input.read(cx).text().trim().to_string();
                let gateway = self.v4_gateway_input.read(cx).text().trim().to_string();
                let metric_str = self.v4_metric_input.read(cx).text().trim().to_string();
                let gateway_metric = if metric_str.is_empty() {
                    0
                } else {
                    metric_str
                        .parse::<u32>()
                        .map_err(|_| format!("IPv4 网关跃点数必须是非负整数: {}", metric_str))?
                };
                let dns_raw = self.v4_dns_input.read(cx).text();
                let dns: Vec<String> = dns_raw
                    .split([',', ';', '，', ' '])
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                Ipv4Config::Static {
                    ip,
                    subnet_mask,
                    gateway,
                    gateway_metric,
                    dns,
                }
            }
        };

        profile.ipv6 = match self.v6_kind {
            Ipv6Kind::Dhcp => Ipv6Config::Dhcp,
            Ipv6Kind::Disabled => Ipv6Config::Disabled,
            Ipv6Kind::Static => {
                let ip = self.v6_ip_input.read(cx).text().trim().to_string();
                let prefix_str = self.v6_prefix_input.read(cx).text().trim().to_string();
                let prefix_length = if prefix_str.is_empty() {
                    64
                } else {
                    prefix_str
                        .parse::<u8>()
                        .map_err(|_| format!("IPv6 前缀长度必须是 1~128 的整数: {}", prefix_str))?
                };
                let gateway = self.v6_gateway_input.read(cx).text().trim().to_string();
                let dns_raw = self.v6_dns_input.read(cx).text();
                let dns: Vec<String> = dns_raw
                    .split([',', ';', '，', ' '])
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                Ipv6Config::Static {
                    ip,
                    prefix_length,
                    gateway,
                    dns,
                }
            }
        };

        Ok(())
    }

    fn load_selected_profile_into_editor(&mut self, cx: &mut Context<Self>) {
        let profile = self
            .selected_profile_id
            .as_deref()
            .and_then(|id| self.draft_config.profiles.iter().find(|p| p.id == id))
            .cloned();

        let fields = extract_editor_fields(profile.as_ref());
        self.v4_kind = fields.v4_kind;
        self.v4_renew = fields.v4_renew;
        self.v6_kind = fields.v6_kind;

        self.name_input.update(cx, |i, cx| i.set_text(fields.name, cx));
        self.v4_ip_input
            .update(cx, |i, cx| i.set_text(fields.v4_ip, cx));
        self.v4_mask_input
            .update(cx, |i, cx| i.set_text(fields.v4_mask, cx));
        self.v4_gateway_input
            .update(cx, |i, cx| i.set_text(fields.v4_gateway, cx));
        self.v4_metric_input
            .update(cx, |i, cx| i.set_text(fields.v4_metric, cx));
        self.v4_dns_input
            .update(cx, |i, cx| i.set_text(fields.v4_dns, cx));

        self.v6_ip_input
            .update(cx, |i, cx| i.set_text(fields.v6_ip, cx));
        self.v6_prefix_input
            .update(cx, |i, cx| i.set_text(fields.v6_prefix, cx));
        self.v6_gateway_input
            .update(cx, |i, cx| i.set_text(fields.v6_gateway, cx));
        self.v6_dns_input
            .update(cx, |i, cx| i.set_text(fields.v6_dns, cx));
    }

    fn refresh_adapters(&mut self, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        self.adapters = list_adapters();
        self.current_status = query_interface_status(
            &self.draft_config.active_interface,
            &self.draft_config.profiles,
        );
        self.banner = Some(Banner::Info(
            format!("已刷新网卡列表，共检测到 {} 张网络适配器", self.adapters.len()).into(),
        ));
        cx.notify();
    }

    fn select_adapter(&mut self, name: String, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        self.draft_config.active_interface = name.clone();
        self.current_status = query_interface_status(&name, &self.draft_config.profiles);

        self.selected_profile_id = self
            .current_status
            .matched_profile_id
            .clone()
            .or_else(|| {
                self.draft_config
                    .profiles_for_interface(&name)
                    .first()
                    .map(|p| p.id.clone())
            });

        self.load_selected_profile_into_editor(cx);
        self.banner = Some(Banner::Info(
            format!(
                "已切换目标网卡: {}（{} 套 Profile）",
                name,
                self.draft_config.profiles_for_interface(&name).len()
            )
            .into(),
        ));
        cx.notify();
    }

    fn select_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        self.selected_profile_id = Some(id);
        self.load_selected_profile_into_editor(cx);
        cx.notify();
    }

    fn add_profile(&mut self, default_dhcp: bool, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        let iface = self.draft_config.active_interface.clone();
        let existing_count = self.draft_config.profiles_for_interface(&iface).len();

        let (id, name, ipv4, ipv6) = if default_dhcp {
            (
                self.draft_config.next_profile_id("dhcp"),
                if existing_count == 0 {
                    "DHCP 自动获取".to_string()
                } else {
                    format!("DHCP 自动 {}", existing_count + 1)
                },
                Ipv4Config::Dhcp {
                    renew_on_switch: true,
                },
                Ipv6Config::Dhcp,
            )
        } else {
            let adapter = self
                .adapters
                .iter()
                .find(|a| a.friendly_name.eq_ignore_ascii_case(&iface));
            let ip = adapter
                .and_then(|a| a.ipv4.clone())
                .unwrap_or_else(|| "192.168.1.100".to_string());
            let mask = adapter
                .and_then(|a| a.subnet_mask.clone())
                .unwrap_or_else(|| "255.255.255.0".to_string());
            let gw = adapter
                .and_then(|a| a.gateway.clone())
                .unwrap_or_else(|| "192.168.1.2".to_string());
            let dns = adapter
                .map(|a| a.dns.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| vec!["192.168.1.1".to_string()]);
            (
                self.draft_config.next_profile_id("static"),
                format!("静态配置 {}", existing_count + 1),
                Ipv4Config::Static {
                    ip,
                    subnet_mask: mask,
                    gateway: gw,
                    gateway_metric: 0,
                    dns,
                },
                Ipv6Config::Disabled,
            )
        };

        let profile = NetworkProfile {
            id: id.clone(),
            name: name.clone(),
            interface_name: iface,
            ipv4,
            ipv6,
        };
        self.draft_config.profiles.push(profile);
        self.selected_profile_id = Some(id);
        self.load_selected_profile_into_editor(cx);
        self.banner = Some(Banner::Info(
            format!("已新建 Profile「{}」，可在右侧分别配置 IPv4 / IPv6 后保存或应用", name).into(),
        ));
        cx.notify();
    }

    fn duplicate_selected_profile(&mut self, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        let Some(selected_id) = self.selected_profile_id.clone() else {
            return;
        };
        let Some(orig) = self
            .draft_config
            .profiles
            .iter()
            .find(|p| p.id == selected_id)
            .cloned()
        else {
            return;
        };

        let new_id = self.draft_config.next_profile_id("profile");
        let new_name = format!("{} 副本", orig.name);
        let copy = NetworkProfile {
            id: new_id.clone(),
            name: new_name.clone(),
            interface_name: orig.interface_name,
            ipv4: orig.ipv4,
            ipv6: orig.ipv6,
        };
        self.draft_config.profiles.push(copy);
        self.selected_profile_id = Some(new_id);
        self.load_selected_profile_into_editor(cx);
        self.banner = Some(Banner::Info(
            format!("已复制为新 Profile「{}」", new_name).into(),
        ));
        cx.notify();
    }

    fn delete_selected_profile(&mut self, cx: &mut Context<Self>) {
        let Some(selected_id) = self.selected_profile_id.clone() else {
            return;
        };
        let removed_name = self
            .draft_config
            .profiles
            .iter()
            .find(|p| p.id == selected_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();

        self.draft_config.profiles.retain(|p| p.id != selected_id);
        self.selected_profile_id = self
            .draft_config
            .active_profiles()
            .first()
            .map(|p| p.id.clone());
        self.load_selected_profile_into_editor(cx);
        self.current_status = query_interface_status(
            &self.draft_config.active_interface,
            &self.draft_config.profiles,
        );
        self.banner = Some(Banner::Info(
            format!("已删除 Profile「{}」（点击保存生效）", removed_name).into(),
        ));
        cx.notify();
    }

    fn set_v4_kind(&mut self, kind: Ipv4Kind, cx: &mut Context<Self>) {
        if self.v4_kind == kind {
            return;
        }
        self.v4_kind = kind;
        let _ = self.sync_editor_into_draft(cx);
        cx.notify();
    }

    fn set_v6_kind(&mut self, kind: Ipv6Kind, cx: &mut Context<Self>) {
        if self.v6_kind == kind {
            return;
        }
        self.v6_kind = kind;
        let _ = self.sync_editor_into_draft(cx);
        cx.notify();
    }

    fn import_current_ipv4(&mut self, cx: &mut Context<Self>) {
        let iface = &self.draft_config.active_interface;
        let Some(adapter) = self
            .adapters
            .iter()
            .find(|a| a.friendly_name.eq_ignore_ascii_case(iface))
            .cloned()
        else {
            self.banner = Some(Banner::Error("当前选中的网卡不在系统列表中".into()));
            cx.notify();
            return;
        };

        self.v4_kind = Ipv4Kind::Static;
        if let Some(ip) = adapter.ipv4 {
            self.v4_ip_input.update(cx, |i, cx| i.set_text(ip, cx));
        }
        if let Some(mask) = adapter.subnet_mask {
            self.v4_mask_input.update(cx, |i, cx| i.set_text(mask, cx));
        }
        if let Some(gw) = adapter.gateway {
            self.v4_gateway_input.update(cx, |i, cx| i.set_text(gw, cx));
        }
        if !adapter.dns.is_empty() {
            let dns_str = adapter.dns.join(", ");
            self.v4_dns_input
                .update(cx, |i, cx| i.set_text(dns_str, cx));
        }
        let _ = self.sync_editor_into_draft(cx);
        self.banner = Some(Banner::Info(
            format!("已从 {} 导入当前 IPv4 参数（仅写入表单，未修改网卡）", adapter.friendly_name)
                .into(),
        ));
        cx.notify();
    }

    fn import_current_ipv6(&mut self, cx: &mut Context<Self>) {
        let iface = &self.draft_config.active_interface;
        let Some(adapter) = self
            .adapters
            .iter()
            .find(|a| a.friendly_name.eq_ignore_ascii_case(iface))
            .cloned()
        else {
            self.banner = Some(Banner::Error("当前选中的网卡不在系统列表中".into()));
            cx.notify();
            return;
        };

        self.v6_kind = Ipv6Kind::Static;
        if let Some(ip) = adapter.ipv6 {
            self.v6_ip_input.update(cx, |i, cx| i.set_text(ip, cx));
        }
        if let Some(prefix) = adapter.ipv6_prefix {
            self.v6_prefix_input
                .update(cx, |i, cx| i.set_text(prefix.to_string(), cx));
        }
        if let Some(gw) = adapter.ipv6_gateway {
            self.v6_gateway_input.update(cx, |i, cx| i.set_text(gw, cx));
        }
        if !adapter.ipv6_dns.is_empty() {
            let dns_str = adapter.ipv6_dns.join(", ");
            self.v6_dns_input
                .update(cx, |i, cx| i.set_text(dns_str, cx));
        }
        let _ = self.sync_editor_into_draft(cx);
        self.banner = Some(Banner::Info(
            format!("已从 {} 导入当前 IPv6 参数（仅写入表单，未修改网卡）", adapter.friendly_name)
                .into(),
        ));
        cx.notify();
    }

    fn reset_from_saved(&mut self, cx: &mut Context<Self>) {
        if let Ok(reloaded) = load_config(&self.config_path) {
            self.saved_config = reloaded;
        }
        self.draft_config = self.saved_config.clone();
        self.show_notifications = self.draft_config.ui.show_notifications;
        self.interval_input.update(cx, |i, cx| {
            i.set_text(self.draft_config.ui.refresh_interval_seconds.to_string(), cx)
        });
        self.current_status = query_interface_status(
            &self.draft_config.active_interface,
            &self.draft_config.profiles,
        );
        self.selected_profile_id = self
            .current_status
            .matched_profile_id
            .clone()
            .or_else(|| {
                self.draft_config
                    .active_profiles()
                    .first()
                    .map(|p| p.id.clone())
            });
        self.load_selected_profile_into_editor(cx);
        self.banner = Some(Banner::Info("已恢复为磁盘中保存的配置".into()));
        cx.notify();
    }

    fn save_all(&mut self, cx: &mut Context<Self>) -> Option<Config> {
        if let Err(e) = self.sync_editor_into_draft(cx) {
            self.banner = Some(Banner::Error(e.into()));
            cx.notify();
            return None;
        }
        if let Err(e) = validate_config(&self.draft_config) {
            self.banner = Some(Banner::Error(e.into()));
            cx.notify();
            return None;
        }

        match save_config(&self.config_path, &self.draft_config) {
            Ok(()) => {
                self.saved_config = self.draft_config.clone();
                self.current_status = query_interface_status(
                    &self.draft_config.active_interface,
                    &self.draft_config.profiles,
                );
                let _ = notify_tray_message(TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG);
                self.banner = Some(Banner::Success(
                    "配置已保存至 config.json 并同步到托盘（尚未改动网卡，点击「切换」或「保存并应用」生效）"
                        .into(),
                ));
                cx.notify();
                Some(self.draft_config.clone())
            }
            Err(e) => {
                self.banner = Some(Banner::Error(e.into()));
                cx.notify();
                None
            }
        }
    }

    fn apply_selected_profile(&mut self, save_first: bool, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_profile_id.clone() {
            self.apply_profile_by_id(id, save_first, cx);
        } else {
            self.banner = Some(Banner::Error("请先在中间栏选择或新建一套 Profile".into()));
            cx.notify();
        }
    }

    fn apply_profile_by_id(&mut self, target_id: String, save_first: bool, cx: &mut Context<Self>) {
        if self.switching_profile_name.is_some() {
            return;
        }
        if !self.is_admin {
            self.banner = Some(Banner::Error(
                "当前不是管理员权限，无法修改网卡配置；请以管理员身份运行".into(),
            ));
            cx.notify();
            return;
        }

        if save_first {
            if self.save_all(cx).is_none() {
                return;
            }
        } else if let Err(e) = self.sync_editor_into_draft(cx) {
            self.banner = Some(Banner::Error(e.into()));
            cx.notify();
            return;
        }

        if self.selected_profile_id.as_deref() != Some(target_id.as_str()) {
            self.selected_profile_id = Some(target_id.clone());
            self.load_selected_profile_into_editor(cx);
        }

        let Some(profile) = self
            .draft_config
            .profiles
            .iter()
            .find(|p| p.id == target_id)
            .cloned()
        else {
            self.banner = Some(Banner::Error("未找到目标 Profile".into()));
            cx.notify();
            return;
        };

        if let Err(e) = validate_profile(&profile) {
            self.banner = Some(Banner::Error(e.into()));
            cx.notify();
            return;
        }

        if validate_config(&self.draft_config).is_ok()
            && save_config(&self.config_path, &self.draft_config).is_ok()
        {
            self.saved_config = self.draft_config.clone();
        }

        let all_profiles = self.draft_config.profiles.clone();
        let target_name = profile.name.clone();
        let mode_summary = profile.short_mode_summary();
        self.switching_profile_name = Some(target_name.clone());
        self.banner = Some(Banner::Info(
            format!(
                "正在将 {} 切换到「{}」[{}]（统一应用 IPv4 与 IPv6 配置）...",
                profile.interface_name, target_name, mode_summary
            )
            .into(),
        ));
        cx.notify();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { apply_profile(&profile, &all_profiles) })
                .await;

            let _ = view.update(cx, |this, cx| {
                this.switching_profile_name = None;
                this.adapters = list_adapters();
                match result {
                    Ok(status) => {
                        this.current_status = status;
                        let _ = notify_tray_message(TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG);
                        this.banner = Some(Banner::Success(
                            format!(
                                "已切换到「{}」（{}）",
                                target_name,
                                this.current_status.summary_line()
                            )
                            .into(),
                        ));
                    }
                    Err(err) => {
                        this.current_status = query_interface_status(
                            &this.draft_config.active_interface,
                            &this.draft_config.profiles,
                        );
                        this.banner = Some(Banner::Error(err.into()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_adapter_card(&self, adapter: &AdapterSummary, cx: &mut Context<Self>) -> AnyElement {
        let selected = adapter
            .friendly_name
            .eq_ignore_ascii_case(&self.draft_config.active_interface);
        let name_for_click = adapter.friendly_name.clone();
        let profile_count = self
            .draft_config
            .profiles_for_interface(&adapter.friendly_name)
            .len();

        let (dot_color, state_label, state_bg) = if adapter.is_up {
            (rgb(0x10b981), "在线", rgb(0x064e3b))
        } else {
            (rgb(0x64748b), "断开", rgb(0x334155))
        };

        let v4_text = if !adapter.ipv4_enabled {
            "v4关".to_string()
        } else {
            format!(
                "{}({})",
                adapter.ipv4.as_deref().unwrap_or("-"),
                if adapter.dhcp_enabled { "DHCP" } else { "静态" }
            )
        };
        let v6_text = if adapter.ipv6_enabled { "v6开" } else { "v6关" };

        div()
            .id(SharedString::from(format!("adapter-{}", adapter.friendly_name)))
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(10.))
            .rounded(px(8.))
            .bg(if selected { rgb(0x172554) } else { rgb(0x0f172a) })
            .border_1()
            .border_color(if selected { rgb(0x3b82f6) } else { rgb(0x1e293b) })
            .cursor_pointer()
            .hover(|s| s.border_color(rgb(0x60a5fa)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_adapter(name_for_click.clone(), cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .flex_1()
                            .min_w(px(0.))
                            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(dot_color))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xf8fafc))
                                    .child(adapter.friendly_name.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .gap(px(4.))
                            .child(
                                div()
                                    .px(px(5.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(state_bg)
                                    .text_size(px(10.))
                                    .text_color(rgb(0xe2e8f0))
                                    .child(state_label),
                            )
                            .child(
                                div()
                                    .px(px(5.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(rgb(0x1e293b))
                                    .text_size(px(10.))
                                    .text_color(rgb(0x93c5fd))
                                    .child(format!("{}套", profile_count)),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(rgb(0x94a3b8))
                    .child(adapter.description.clone()),
            )
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(rgb(0xcbd5e1))
                    .child(format!("{} · {}", v4_text, v6_text)),
            )
            .into_any_element()
    }

    fn render_profile_card(&self, profile: &NetworkProfile, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_profile_id.as_deref() == Some(profile.id.as_str());
        let active_now =
            self.current_status.matched_profile_id.as_deref() == Some(profile.id.as_str());
        let is_switching_any = self.switching_profile_name.is_some();
        let is_switching_this =
            self.switching_profile_name.as_deref() == Some(profile.name.as_str());
        let id_for_select = profile.id.clone();
        let id_for_apply = profile.id.clone();

        let v4_badge_bg = match profile.ipv4 {
            Ipv4Config::Static { .. } => rgb(0x1d4ed8),
            Ipv4Config::Dhcp { .. } => rgb(0x065f46),
            Ipv4Config::Disabled => rgb(0x475569),
        };
        let v6_badge_bg = match profile.ipv6 {
            Ipv6Config::Static { .. } => rgb(0x6d28d9),
            Ipv6Config::Dhcp => rgb(0x0f766e),
            Ipv6Config::Disabled => rgb(0x9a3412),
        };

        div()
            .id(SharedString::from(format!("profile-{}", profile.id)))
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(5.))
            .p(px(10.))
            .rounded(px(8.))
            .bg(if selected { rgb(0x172554) } else { rgb(0x0f172a) })
            .border_1()
            .border_color(if active_now {
                rgb(0x10b981)
            } else if selected {
                rgb(0x3b82f6)
            } else {
                rgb(0x1e293b)
            })
            .cursor_pointer()
            .hover(|s| s.border_color(rgb(0x60a5fa)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_profile(id_for_select.clone(), cx);
                }),
            )
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xf8fafc))
                    .child(if profile.name.is_empty() {
                        "(未命名 Profile)".to_string()
                    } else {
                        profile.name.clone()
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .px(px(5.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(v4_badge_bg)
                            .text_size(px(10.))
                            .text_color(rgb(0xf8fafc))
                            .child(profile.ipv4.short_label()),
                    )
                    .child(
                        div()
                            .px(px(5.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(v6_badge_bg)
                            .text_size(px(10.))
                            .text_color(rgb(0xf8fafc))
                            .child(profile.ipv6.short_label()),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(rgb(0x94a3b8))
                    .child(profile.ipv4.summary()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt(px(2.))
                    .child(if active_now {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(div().size(px(6.)).rounded_full().bg(rgb(0x34d399)))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(0x34d399))
                                    .child("当前生效中"),
                            )
                    } else {
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(0x64748b))
                            .child("未激活")
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("btn-switch-{}", profile.id)))
                            .px(px(9.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .cursor_pointer()
                            .bg(if is_switching_this {
                                rgb(0xd97706)
                            } else if is_switching_any {
                                rgb(0x334155)
                            } else if active_now {
                                rgb(0x1e293b)
                            } else {
                                rgb(0x2563eb)
                            })
                            .border_1()
                            .border_color(if active_now {
                                rgb(0x334155)
                            } else if is_switching_any {
                                rgb(0x475569)
                            } else {
                                rgb(0x60a5fa)
                            })
                            .text_color(if active_now {
                                rgb(0x94a3b8)
                            } else {
                                rgb(0xffffff)
                            })
                            .hover(|s| {
                                if active_now {
                                    s.bg(rgb(0x334155)).text_color(rgb(0xf8fafc))
                                } else {
                                    s.bg(rgb(0x1d4ed8))
                                }
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.apply_profile_by_id(id_for_apply.clone(), false, cx);
                                }),
                            )
                            .child(if is_switching_this {
                                "切换中..."
                            } else if active_now {
                                "重应用"
                            } else {
                                "切换"
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_field(label: &str, input: Entity<TextInput>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .flex_1()
            .min_w(px(0.))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0x94a3b8))
                    .child(label.to_string()),
            )
            .child(input)
            .into_any_element()
    }

    fn render_toggle_row(
        id: &'static str,
        label: &str,
        checked: bool,
        on_toggle: impl Fn(&mut Self, &gpui::MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .w_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .py(px(4.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(on_toggle))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.))
                    .text_color(rgb(0xe2e8f0))
                    .child(label.to_string()),
            )
            .child(
                div()
                    .w(px(38.))
                    .h(px(20.))
                    .flex_shrink_0()
                    .rounded_full()
                    .p(px(2.))
                    .flex()
                    .items_center()
                    .bg(if checked { rgb(0x2563eb) } else { rgb(0x334155) })
                    .justify_end()
                    .when(!checked, |this| this.justify_start())
                    .child(div().size(px(16.)).rounded_full().bg(rgb(0xffffff))),
            )
            .into_any_element()
    }

    fn render_seg_btn(
        id: &'static str,
        label: &'static str,
        active: bool,
        active_bg: u32,
        active_border: u32,
        on_click: impl Fn(&mut Self, &gpui::MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .flex_1()
            .min_w(px(0.))
            .py(px(6.))
            .px(px(6.))
            .rounded(px(6.))
            .text_center()
            .whitespace_nowrap()
            .text_size(px(12.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .cursor_pointer()
            .bg(if active { rgb(active_bg) } else { rgb(0x0f172a) })
            .border_1()
            .border_color(if active {
                rgb(active_border)
            } else {
                rgb(0x334155)
            })
            .text_color(rgb(0xf8fafc))
            .on_mouse_down(MouseButton::Left, cx.listener(on_click))
            .child(label)
            .into_any_element()
    }

    fn render_editor_card(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selected_profile_id.is_none() {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .p(px(28.))
                .rounded(px(10.))
                .bg(rgb(0x111827))
                .border_1()
                .border_color(rgb(0x1e293b))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(rgb(0x94a3b8))
                        .child(format!(
                            "网卡「{}」还没有配置任何 Profile",
                            self.draft_config.active_interface
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            div()
                                .id("empty-add-static")
                                .px(px(12.))
                                .py(px(7.))
                                .rounded(px(6.))
                                .bg(rgb(0x2563eb))
                                .text_size(px(12.))
                                .text_color(rgb(0xffffff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(0x1d4ed8)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.add_profile(false, cx)),
                                )
                                .child("+ 新建静态 Profile"),
                        )
                        .child(
                            div()
                                .id("empty-add-dhcp")
                                .px(px(12.))
                                .py(px(7.))
                                .rounded(px(6.))
                                .bg(rgb(0x059669))
                                .text_size(px(12.))
                                .text_color(rgb(0xffffff))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(0x047857)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.add_profile(true, cx)),
                                )
                                .child("+ 新建 DHCP Profile"),
                        ),
                )
                .into_any_element();
        }

        // IPv4 segmented buttons + detail form
        let v4_btn_static = Self::render_seg_btn(
            "v4-mode-static",
            "静态 IP",
            self.v4_kind == Ipv4Kind::Static,
            0x2563eb,
            0x60a5fa,
            |this, _, _, cx| this.set_v4_kind(Ipv4Kind::Static, cx),
            cx,
        );
        let v4_btn_dhcp = Self::render_seg_btn(
            "v4-mode-dhcp",
            "DHCP 自动",
            self.v4_kind == Ipv4Kind::Dhcp,
            0x059669,
            0x34d399,
            |this, _, _, cx| this.set_v4_kind(Ipv4Kind::Dhcp, cx),
            cx,
        );
        let v4_btn_disabled = Self::render_seg_btn(
            "v4-mode-disabled",
            "关闭 IPv4",
            self.v4_kind == Ipv4Kind::Disabled,
            0xc2410c,
            0xfb923c,
            |this, _, _, cx| this.set_v4_kind(Ipv4Kind::Disabled, cx),
            cx,
        );

        let v4_detail: AnyElement = match self.v4_kind {
            Ipv4Kind::Static => div()
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field("IPv4 地址", self.v4_ip_input.clone()))
                        .child(Self::render_field("子网掩码", self.v4_mask_input.clone())),
                )
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field(
                            "默认网关 (如旁路由)",
                            self.v4_gateway_input.clone(),
                        ))
                        .child(Self::render_field(
                            "跃点数 (0=自动)",
                            self.v4_metric_input.clone(),
                        )),
                )
                .child(Self::render_field(
                    "IPv4 DNS（多条用英文逗号分隔）",
                    self.v4_dns_input.clone(),
                ))
                .child(
                    div().flex().justify_end().child(
                        div()
                            .id("btn-import-v4")
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(5.))
                            .bg(rgb(0x1e293b))
                            .border_1()
                            .border_color(rgb(0x334155))
                            .text_size(px(11.))
                            .text_color(rgb(0x93c5fd))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x334155)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.import_current_ipv4(cx)),
                            )
                            .child("导入当前网卡 IPv4"),
                    ),
                )
                .into_any_element(),
            Ipv4Kind::Dhcp => div()
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(6.))
                .bg(rgb(0x0b1220))
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .text_size(px(11.))
                        .text_color(rgb(0x94a3b8))
                        .child("应用此 Profile 时，将 IPv4 地址、网关与 DNS 设为自动获取。"),
                )
                .child(Self::render_toggle_row(
                    "toggle-v4-renew",
                    "切换后自动刷新租约 (ipconfig /renew)",
                    self.v4_renew,
                    |this, _, _, cx| {
                        this.v4_renew = !this.v4_renew;
                        let _ = this.sync_editor_into_draft(cx);
                        cx.notify();
                    },
                    cx,
                ))
                .into_any_element(),
            Ipv4Kind::Disabled => div()
                .w_full()
                .min_w(px(0.))
                .p(px(10.))
                .rounded(px(6.))
                .bg(rgb(0x0b1220))
                .text_size(px(11.))
                .text_color(rgb(0xfdba74))
                .child("应用此 Profile 时禁用 TCP/IPv4 (ms_tcpip)，仅在点击切换/应用时生效。")
                .into_any_element(),
        };

        // IPv6 segmented buttons + detail form
        let v6_btn_dhcp = Self::render_seg_btn(
            "v6-mode-dhcp",
            "DHCP / 自动",
            self.v6_kind == Ipv6Kind::Dhcp,
            0x0f766e,
            0x2dd4bf,
            |this, _, _, cx| this.set_v6_kind(Ipv6Kind::Dhcp, cx),
            cx,
        );
        let v6_btn_static = Self::render_seg_btn(
            "v6-mode-static",
            "静态 IPv6",
            self.v6_kind == Ipv6Kind::Static,
            0x6d28d9,
            0xa78bfa,
            |this, _, _, cx| this.set_v6_kind(Ipv6Kind::Static, cx),
            cx,
        );
        let v6_btn_disabled = Self::render_seg_btn(
            "v6-mode-disabled",
            "关闭 IPv6",
            self.v6_kind == Ipv6Kind::Disabled,
            0xc2410c,
            0xfb923c,
            |this, _, _, cx| this.set_v6_kind(Ipv6Kind::Disabled, cx),
            cx,
        );

        let v6_detail: AnyElement = match self.v6_kind {
            Ipv6Kind::Dhcp => div()
                .w_full()
                .min_w(px(0.))
                .p(px(10.))
                .rounded(px(6.))
                .bg(rgb(0x0b1220))
                .text_size(px(11.))
                .text_color(rgb(0x94a3b8))
                .child("应用此 Profile 时启用 IPv6 并设为自动获取 (SLAAC / DHCPv6)。")
                .into_any_element(),
            Ipv6Kind::Static => div()
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field("IPv6 地址", self.v6_ip_input.clone()))
                        .child(
                            div()
                                .w(px(110.))
                                .flex_shrink_0()
                                .child(Self::render_field("前缀长度", self.v6_prefix_input.clone())),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field(
                            "IPv6 默认网关 (可选)",
                            self.v6_gateway_input.clone(),
                        ))
                        .child(Self::render_field(
                            "IPv6 DNS (可选，逗号分隔)",
                            self.v6_dns_input.clone(),
                        )),
                )
                .child(
                    div().flex().justify_end().child(
                        div()
                            .id("btn-import-v6")
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(5.))
                            .bg(rgb(0x1e293b))
                            .border_1()
                            .border_color(rgb(0x334155))
                            .text_size(px(11.))
                            .text_color(rgb(0xc4b5fd))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x334155)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.import_current_ipv6(cx)),
                            )
                            .child("导入当前网卡 IPv6"),
                    ),
                )
                .into_any_element(),
            Ipv6Kind::Disabled => div()
                .w_full()
                .min_w(px(0.))
                .p(px(10.))
                .rounded(px(6.))
                .bg(rgb(0x0b1220))
                .text_size(px(11.))
                .text_color(rgb(0xfdba74))
                .child("应用此 Profile 时禁用 TCP/IPv6 (ms_tcpip6)，仅在点击切换/应用时生效。")
                .into_any_element(),
        };

        div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(14.))
            .rounded(px(10.))
            .bg(rgb(0x111827))
            .border_1()
            .border_color(rgb(0x1e293b))
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xe2e8f0))
                            .child("Profile 详细配置"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("btn-dup-profile")
                                    .px(px(9.))
                                    .py(px(4.))
                                    .rounded(px(5.))
                                    .bg(rgb(0x1e293b))
                                    .border_1()
                                    .border_color(rgb(0x334155))
                                    .text_size(px(11.))
                                    .text_color(rgb(0xcbd5e1))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x334155)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.duplicate_selected_profile(cx);
                                        }),
                                    )
                                    .child("复制"),
                            )
                            .child(
                                div()
                                    .id("btn-del-profile")
                                    .px(px(9.))
                                    .py(px(4.))
                                    .rounded(px(5.))
                                    .bg(rgb(0x450a0a))
                                    .border_1()
                                    .border_color(rgb(0x991b1b))
                                    .text_size(px(11.))
                                    .text_color(rgb(0xfca5a5))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x7f1d1d)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.delete_selected_profile(cx);
                                        }),
                                    )
                                    .child("删除"),
                            ),
                    ),
            )
            .child(Self::render_field("Profile 名称", self.name_input.clone()))
            // IPv4 Section
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(11.))
                    .rounded(px(8.))
                    .bg(rgb(0x0f172a))
                    .border_1()
                    .border_color(rgb(0x1e293b))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0x93c5fd))
                            .child("IPv4 配置"),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .flex()
                            .gap(px(8.))
                            .child(v4_btn_static)
                            .child(v4_btn_dhcp)
                            .child(v4_btn_disabled),
                    )
                    .child(v4_detail),
            )
            // IPv6 Section
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(11.))
                    .rounded(px(8.))
                    .bg(rgb(0x0f172a))
                    .border_1()
                    .border_color(rgb(0x1e293b))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0x5eead4))
                            .child("IPv6 配置"),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .flex()
                            .gap(px(8.))
                            .child(v6_btn_dhcp)
                            .child(v6_btn_static)
                            .child(v6_btn_disabled),
                    )
                    .child(v6_detail),
            )
            .into_any_element()
    }
}

fn extract_editor_fields(profile: Option<&NetworkProfile>) -> EditorFields {
    let mut fields = EditorFields {
        name: String::new(),
        v4_kind: Ipv4Kind::Static,
        v4_renew: true,
        v4_ip: "192.168.1.100".to_string(),
        v4_mask: "255.255.255.0".to_string(),
        v4_gateway: "192.168.1.2".to_string(),
        v4_metric: "0".to_string(),
        v4_dns: "192.168.1.1".to_string(),
        v6_kind: Ipv6Kind::Dhcp,
        v6_ip: String::new(),
        v6_prefix: "64".to_string(),
        v6_gateway: String::new(),
        v6_dns: String::new(),
    };

    let Some(p) = profile else {
        return fields;
    };

    fields.name = p.name.clone();

    match &p.ipv4 {
        Ipv4Config::Static {
            ip,
            subnet_mask,
            gateway,
            gateway_metric,
            dns,
        } => {
            fields.v4_kind = Ipv4Kind::Static;
            fields.v4_ip = ip.clone();
            fields.v4_mask = subnet_mask.clone();
            fields.v4_gateway = gateway.clone();
            fields.v4_metric = gateway_metric.to_string();
            fields.v4_dns = dns.join(", ");
        }
        Ipv4Config::Dhcp { renew_on_switch } => {
            fields.v4_kind = Ipv4Kind::Dhcp;
            fields.v4_renew = *renew_on_switch;
        }
        Ipv4Config::Disabled => {
            fields.v4_kind = Ipv4Kind::Disabled;
        }
    }

    match &p.ipv6 {
        Ipv6Config::Dhcp => {
            fields.v6_kind = Ipv6Kind::Dhcp;
        }
        Ipv6Config::Disabled => {
            fields.v6_kind = Ipv6Kind::Disabled;
        }
        Ipv6Config::Static {
            ip,
            prefix_length,
            gateway,
            dns,
        } => {
            fields.v6_kind = Ipv6Kind::Static;
            fields.v6_ip = ip.clone();
            fields.v6_prefix = prefix_length.to_string();
            fields.v6_gateway = gateway.clone();
            fields.v6_dns = dns.join(", ");
        }
    }

    fields
}

impl Render for ConfigWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode_color = match self.current_status.mode {
            NetworkMode::Static => rgb(0x2563eb),
            NetworkMode::StaticCustom => rgb(0x7c3aed),
            NetworkMode::Dhcp => rgb(0x059669),
            NetworkMode::Disconnected => rgb(0xdc2626),
        };

        let is_switching = self.switching_profile_name.is_some();

        let mut adapter_cards = Vec::with_capacity(self.adapters.len());
        for adapter in &self.adapters {
            adapter_cards.push(self.render_adapter_card(adapter, cx));
        }

        let active_iface = self.draft_config.active_interface.clone();
        let active_profiles: Vec<NetworkProfile> = self
            .draft_config
            .profiles
            .iter()
            .filter(|p| p.interface_name.eq_ignore_ascii_case(&active_iface))
            .cloned()
            .collect();
        let mut profile_cards = Vec::with_capacity(active_profiles.len());
        for p in &active_profiles {
            profile_cards.push(self.render_profile_card(p, cx));
        }

        let editor_card = self.render_editor_card(cx);
        let toggle_notify = Self::render_toggle_row(
            "toggle-notify",
            "切换完成时弹出右下角气泡通知",
            self.show_notifications,
            |this, _, _, cx| {
                this.show_notifications = !this.show_notifications;
                cx.notify();
            },
            cx,
        );
        let interval_field =
            Self::render_field("后台轮询间隔（秒，最小 3）", self.interval_input.clone());

        let status_badge_text = if let Some(ref target) = self.switching_profile_name {
            format!("正在切换到 {}...", target)
        } else {
            self.current_status.display_label()
        };

        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x0b1120))
            .text_color(rgb(0xf1f5f9))
            // Header
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(18.))
                    .py(px(13.))
                    .bg(rgb(0x111827))
                    .border_b_1()
                    .border_color(rgb(0x1e293b))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(
                                        div()
                                            .text_size(px(16.))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(rgb(0xf8fafc))
                                            .child("Net Switcher · 网卡与 Profile 管理"),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .px(px(8.))
                                            .py(px(2.))
                                            .rounded(px(4.))
                                            .bg(if self.is_admin {
                                                rgb(0x065f46)
                                            } else {
                                                rgb(0x92400e)
                                            })
                                            .text_size(px(11.))
                                            .text_color(rgb(0xf8fafc))
                                            .child(if self.is_admin {
                                                "管理员 · 单实例"
                                            } else {
                                                "非管理员"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(rgb(0x94a3b8))
                                    .child(format!(
                                        "配置文件: {}  ·  关闭窗口后继续常驻右下角托盘",
                                        self.config_path.display()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("btn-refresh-adapters")
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .px(px(12.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .bg(rgb(0x1e293b))
                            .border_1()
                            .border_color(rgb(0x334155))
                            .text_size(px(12.))
                            .text_color(rgb(0xe2e8f0))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x334155)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.refresh_adapters(cx)),
                            )
                            .child("刷新网卡状态"),
                    ),
            )
            // Body (Three Columns)
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .flex()
                    .flex_1()
                    .overflow_hidden()
                    .gap(px(12.))
                    .p(px(14.))
                    // Column 1: Network Adapters
                    .child(
                        div()
                            .w(px(225.))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(11.))
                            .rounded(px(10.))
                            .bg(rgb(0x111827))
                            .border_1()
                            .border_color(rgb(0x1e293b))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xe2e8f0))
                                            .child("1. 选择网卡"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(0x64748b))
                                            .child(format!("{} 张", self.adapters.len())),
                                    ),
                            )
                            .child(
                                div()
                                    .id("adapter-list-scroll")
                                    .w_full()
                                    .min_w(px(0.))
                                    .min_h(px(0.))
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .gap(px(7.))
                                    .overflow_y_scroll()
                                    .children(adapter_cards),
                            ),
                    )
                    // Column 2: Profiles
                    .child(
                        div()
                            .w(px(235.))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(11.))
                            .rounded(px(10.))
                            .bg(rgb(0x111827))
                            .border_1()
                            .border_color(rgb(0x1e293b))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xe2e8f0))
                                            .child("2. 网卡 Profile"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(0x64748b))
                                            .child(format!("{} 套", active_profiles.len())),
                                    ),
                            )
                            .child(
                                div()
                                    .id("profile-list-scroll")
                                    .w_full()
                                    .min_w(px(0.))
                                    .min_h(px(0.))
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .gap(px(7.))
                                    .overflow_y_scroll()
                                    .children(profile_cards),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .id("btn-add-static")
                                            .flex_1()
                                            .py(px(6.))
                                            .rounded(px(6.))
                                            .bg(rgb(0x1e3a8a))
                                            .text_center()
                                            .text_size(px(11.))
                                            .text_color(rgb(0xdbeafe))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(0x1d4ed8)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.add_profile(false, cx);
                                                }),
                                            )
                                            .child("+ 静态模板"),
                                    )
                                    .child(
                                        div()
                                            .id("btn-add-dhcp")
                                            .flex_1()
                                            .py(px(6.))
                                            .rounded(px(6.))
                                            .bg(rgb(0x064e3b))
                                            .text_center()
                                            .text_size(px(11.))
                                            .text_color(rgb(0xd1fae5))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgb(0x047857)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.add_profile(true, cx);
                                                }),
                                            )
                                            .child("+ DHCP 模板"),
                                    ),
                            ),
                    )
                    // Column 3: Live Status (read-only) + Editor + Tray Settings
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .min_h(px(0.))
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("right-config-scroll")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap(px(10.))
                                            .pr(px(4.))
                                    // Live Status Card (read-only display + cycle next)
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap(px(6.))
                                            .p(px(12.))
                                            .rounded(px(10.))
                                            .bg(rgb(0x111827))
                                            .border_1()
                                            .border_color(rgb(0x1e293b))
                                            .child(
                                                div()
                                                    .w_full()
                                                    .min_w(px(0.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap(px(8.))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .gap(px(8.))
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_size(px(13.))
                                                                    .font_weight(
                                                                        gpui::FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(format!(
                                                                        "网卡实时状态: {}",
                                                                        active_iface
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .whitespace_nowrap()
                                                                    .px(px(8.))
                                                                    .py(px(2.))
                                                                    .rounded(px(4.))
                                                                    .bg(mode_color)
                                                                    .text_size(px(11.))
                                                                    .text_color(rgb(0xffffff))
                                                                    .child(status_badge_text),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .w_full()
                                                    .min_w(px(0.))
                                                    .text_size(px(12.))
                                                    .text_color(rgb(0x94a3b8))
                                                    .child(self.current_status.summary_line()),
                                            ),
                                    )
                                    // Profile Editor Card
                                    .child(editor_card)
                                    // Global Tray Behavior Card
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap(px(6.))
                                            .p(px(12.))
                                            .rounded(px(10.))
                                            .bg(rgb(0x111827))
                                            .border_1()
                                            .border_color(rgb(0x1e293b))
                                            .child(
                                                div()
                                                    .text_size(px(13.))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(rgb(0xe2e8f0))
                                                    .child("托盘交互设置"),
                                            )
                                            .child(toggle_notify)
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(0x94a3b8))
                                                    .child("左键点击托盘图标打开配置窗口，右键菜单选择 Profile 切换（已禁用图标快捷切网防误触）"),
                                            )
                                            .child(div().w(px(200.)).pt(px(2.)).child(interval_field)),
                                    ),
                            ),
                        ),
                    ),
            )
            // Footer Action Bar
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(18.))
                    .py(px(12.))
                    .bg(rgb(0x111827))
                    .border_t_1()
                    .border_color(rgb(0x1e293b))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(
                        match &self.banner {
                            Some(Banner::Info(msg)) => {
                                div().truncate().text_color(rgb(0x93c5fd)).child(msg.clone())
                            }
                            Some(Banner::Success(msg)) => {
                                div().truncate().text_color(rgb(0x34d399)).child(msg.clone())
                            }
                            Some(Banner::Error(msg)) => {
                                div().truncate().text_color(rgb(0xf87171)).child(msg.clone())
                            }
                            None => div()
                                .truncate()
                                .text_color(rgb(0x64748b))
                                .child("修改 IPv4/IPv6 仅保存在 Profile，点击「切换 / 应用」时统一生效"),
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("btn-reset")
                                    .whitespace_nowrap()
                                    .px(px(12.))
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(rgb(0x1e293b))
                                    .border_1()
                                    .border_color(rgb(0x334155))
                                    .text_size(px(12.))
                                    .text_color(rgb(0xe2e8f0))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x334155)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.reset_from_saved(cx)),
                                    )
                                    .child("重置"),
                            )
                            .child(
                                div()
                                    .id("btn-save-all")
                                    .whitespace_nowrap()
                                    .px(px(14.))
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(rgb(0x2563eb))
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x1d4ed8)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.save_all(cx);
                                        }),
                                    )
                                    .child("仅保存配置"),
                            )
                            .child(
                                div()
                                    .id("btn-save-and-apply")
                                    .whitespace_nowrap()
                                    .px(px(14.))
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(if is_switching {
                                        rgb(0x334155)
                                    } else {
                                        rgb(0x059669)
                                    })
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(rgb(0xffffff))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x047857)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.apply_selected_profile(true, cx);
                                        }),
                                    )
                                    .child("保存并应用选中 Profile"),
                            ),
                    ),
            )
    }
}

fn open_or_focus_window(
    config_path: &PathBuf,
    active_view: &Rc<RefCell<Option<WeakEntity<ConfigWindow>>>>,
    cx: &mut App,
) {
    if !cx.windows().is_empty() {
        let _ = focus_window_by_title(GUI_WINDOW_TITLE);
        cx.activate(true);
        if let Some(weak) = active_view.borrow().as_ref() {
            let _ = weak.update(cx, |this, cx| this.refresh_live_only(cx));
        }
        return;
    }

    let cfg = load_or_default_config(config_path);
    let bounds = Bounds::centered(None, size(px(1120.0), px(720.0)), cx);
    let path_clone = config_path.clone();

    if let Ok(handle) = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(GUI_WINDOW_TITLE.into()),
                ..Default::default()
            }),
            window_min_size: Some(size(px(920.0), px(580.0))),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let view = ConfigWindow::new(cx, cfg, path_clone);
                window.focus(&view.focus_handle);
                view
            })
        },
    ) {
        if let Ok(weak) = handle.update(cx, |_, _, cx| cx.entity().downgrade()) {
            *active_view.borrow_mut() = Some(weak);
        }
        cx.activate(true);
    }
}

pub fn run_app(config_path: PathBuf, gui_rx: Receiver<GuiCommand>, open_on_start: bool) {
    let rx_shared = Arc::new(Mutex::new(gui_rx));

    Application::new().run(move |cx: &mut App| {
        bind_text_input_keys(cx);
        cx.bind_keys([KeyBinding::new("ctrl-w", CloseWindow, None)]);

        let active_view: Rc<RefCell<Option<WeakEntity<ConfigWindow>>>> =
            Rc::new(RefCell::new(None));

        // Do NOT quit app when closing the config window; keep this single process resident in tray
        let active_view_for_close = active_view.clone();
        cx.on_window_closed(move |cx| {
            if cx.windows().is_empty() {
                *active_view_for_close.borrow_mut() = None;
            }
        })
        .detach();

        if open_on_start {
            open_or_focus_window(&config_path, &active_view, cx);
        }

        let path_for_loop = config_path.clone();
        let active_view_for_loop = active_view.clone();
        cx.spawn(async move |cx: &mut AsyncApp| loop {
            let rx = rx_shared.clone();
            let cmd = cx
                .background_executor()
                .spawn(async move { rx.lock().ok().and_then(|r| r.recv().ok()) })
                .await;

            match cmd {
                Some(GuiCommand::OpenOrFocus) => {
                    let path = path_for_loop.clone();
                    let av = active_view_for_loop.clone();
                    let _ = cx.update(|cx| open_or_focus_window(&path, &av, cx));
                }
                Some(GuiCommand::Refresh) => {
                    let av = active_view_for_loop.clone();
                    let _ = cx.update(|cx| {
                        if let Some(weak) = av.borrow().as_ref() {
                            let _ = weak.update(cx, |this, cx| this.refresh_live_only(cx));
                        }
                    });
                }
                Some(GuiCommand::Quit) | None => {
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        })
        .detach();
    });
}
