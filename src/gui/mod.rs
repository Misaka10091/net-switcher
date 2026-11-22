mod text_input;

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding,
    MouseButton, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div,
    prelude::*, px, rgb, size,
};

use crate::config::{
    Config, NetworkProfile, ProfileMode, load_config, save_config, validate_config,
    validate_profile,
};
use crate::net::{
    AdapterSummary, NetworkMode, NetworkStatus, apply_profile, list_adapters,
    query_interface_status,
};
use crate::tray::{TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG};
use crate::winutil::{is_elevated, notify_tray_message};

use text_input::{TextInput, bind_text_input_keys};

pub const GUI_WINDOW_TITLE: &str = "Net Switcher — 网卡与 Profile 配置";

actions!(net_gui, [CloseWindow]);

#[derive(Clone)]
enum Banner {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
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

    // Profile editor inputs
    name_input: Entity<TextInput>,
    editor_is_dhcp: bool,
    editor_renew_on_switch: bool,
    ip_input: Entity<TextInput>,
    mask_input: Entity<TextInput>,
    gateway_input: Entity<TextInput>,
    metric_input: Entity<TextInput>,
    dns_input: Entity<TextInput>,

    // Global UI settings inputs
    interval_input: Entity<TextInput>,
    show_notifications: bool,
    left_click_toggle: bool,
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
            .or_else(|| {
                config
                    .active_profiles()
                    .first()
                    .map(|p| p.id.clone())
            });

        let initial_profile = initial_profile_id
            .as_deref()
            .and_then(|id| config.profiles.iter().find(|p| p.id == id))
            .cloned();

        let (
            init_name,
            init_is_dhcp,
            init_renew,
            init_ip,
            init_mask,
            init_gw,
            init_metric,
            init_dns,
        ) = extract_profile_editor_fields(initial_profile.as_ref());

        let name_input = cx.new(|cx| TextInput::new(cx, init_name, "如 旁路由固定网关 / DHCP 自动"));
        let ip_input = cx.new(|cx| TextInput::new(cx, init_ip, "如 192.168.1.100"));
        let mask_input = cx.new(|cx| TextInput::new(cx, init_mask, "如 255.255.255.0"));
        let gateway_input = cx.new(|cx| TextInput::new(cx, init_gw, "如 192.168.1.2"));
        let metric_input = cx.new(|cx| TextInput::new(cx, init_metric, "0 表示自动跃点"));
        let dns_input = cx.new(|cx| TextInput::new(cx, init_dns, "如 192.168.1.1, 223.5.5.5"));
        let interval_input = cx.new(|cx| {
            TextInput::new(cx, config.ui.refresh_interval_seconds.to_string(), "默认 15")
        });

        let show_notifications = config.ui.show_notifications;
        let left_click_toggle = config.ui.left_click_toggle;

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
            editor_is_dhcp: init_is_dhcp,
            editor_renew_on_switch: init_renew,
            ip_input,
            mask_input,
            gateway_input,
            metric_input,
            dns_input,
            interval_input,
            show_notifications,
            left_click_toggle,
        }
    }

    fn sync_editor_into_draft(&mut self, cx: &App) -> Result<(), String> {
        let interval_str = self.interval_input.read(cx).text().trim().to_string();
        if let Ok(v) = interval_str.parse::<u32>() {
            self.draft_config.ui.refresh_interval_seconds = v.max(3);
        }
        self.draft_config.ui.show_notifications = self.show_notifications;
        self.draft_config.ui.left_click_toggle = self.left_click_toggle;

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

        if self.editor_is_dhcp {
            profile.mode = ProfileMode::Dhcp {
                renew_on_switch: self.editor_renew_on_switch,
            };
        } else {
            let ip = self.ip_input.read(cx).text().trim().to_string();
            let subnet_mask = self.mask_input.read(cx).text().trim().to_string();
            let gateway = self.gateway_input.read(cx).text().trim().to_string();
            let metric_str = self.metric_input.read(cx).text().trim().to_string();
            let gateway_metric = if metric_str.is_empty() {
                0
            } else {
                metric_str
                    .parse::<u32>()
                    .map_err(|_| format!("网关跃点数必须是非负整数: {}", metric_str))?
            };
            let dns_raw = self.dns_input.read(cx).text();
            let dns: Vec<String> = dns_raw
                .split([',', ';', '，', ' '])
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();

            profile.mode = ProfileMode::Static {
                ip,
                subnet_mask,
                gateway,
                gateway_metric,
                dns,
            };
        }

        Ok(())
    }

    fn load_selected_profile_into_editor(&mut self, cx: &mut Context<Self>) {
        let profile = self
            .selected_profile_id
            .as_deref()
            .and_then(|id| self.draft_config.profiles.iter().find(|p| p.id == id))
            .cloned();

        let (name, is_dhcp, renew, ip, mask, gw, metric, dns) =
            extract_profile_editor_fields(profile.as_ref());

        self.editor_is_dhcp = is_dhcp;
        self.editor_renew_on_switch = renew;
        self.name_input.update(cx, |i, cx| i.set_text(name, cx));
        self.ip_input.update(cx, |i, cx| i.set_text(ip, cx));
        self.mask_input.update(cx, |i, cx| i.set_text(mask, cx));
        self.gateway_input.update(cx, |i, cx| i.set_text(gw, cx));
        self.metric_input.update(cx, |i, cx| i.set_text(metric, cx));
        self.dns_input.update(cx, |i, cx| i.set_text(dns, cx));
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
        self.current_status =
            query_interface_status(&name, &self.draft_config.profiles);

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

    fn add_profile(&mut self, is_dhcp: bool, cx: &mut Context<Self>) {
        let _ = self.sync_editor_into_draft(cx);
        let iface = self.draft_config.active_interface.clone();
        let existing_count = self.draft_config.profiles_for_interface(&iface).len();

        let (id, name, mode) = if is_dhcp {
            (
                self.draft_config.next_profile_id("dhcp"),
                if existing_count == 0 {
                    "DHCP 自动获取".to_string()
                } else {
                    format!("DHCP 自动 {}", existing_count + 1)
                },
                ProfileMode::Dhcp {
                    renew_on_switch: true,
                },
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
                ProfileMode::Static {
                    ip,
                    subnet_mask: mask,
                    gateway: gw,
                    gateway_metric: 0,
                    dns,
                },
            )
        };

        let profile = NetworkProfile {
            id: id.clone(),
            name: name.clone(),
            interface_name: iface,
            mode,
        };
        self.draft_config.profiles.push(profile);
        self.selected_profile_id = Some(id);
        self.load_selected_profile_into_editor(cx);
        self.banner = Some(Banner::Info(
            format!("已新建 Profile「{}」，编辑完成后请点击保存", name).into(),
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
            mode: orig.mode,
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

    fn set_editor_mode(&mut self, is_dhcp: bool, cx: &mut Context<Self>) {
        if self.editor_is_dhcp == is_dhcp {
            return;
        }
        self.editor_is_dhcp = is_dhcp;
        let _ = self.sync_editor_into_draft(cx);
        cx.notify();
    }

    fn import_selected_adapter_info(&mut self, cx: &mut Context<Self>) {
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

        self.editor_is_dhcp = false;
        if let Some(ip) = adapter.ipv4 {
            self.ip_input.update(cx, |i, cx| i.set_text(ip, cx));
        }
        if let Some(mask) = adapter.subnet_mask {
            self.mask_input.update(cx, |i, cx| i.set_text(mask, cx));
        }
        if let Some(gw) = adapter.gateway {
            self.gateway_input.update(cx, |i, cx| i.set_text(gw, cx));
        }
        if !adapter.dns.is_empty() {
            let dns_str = adapter.dns.join(", ");
            self.dns_input.update(cx, |i, cx| i.set_text(dns_str, cx));
        }
        let _ = self.sync_editor_into_draft(cx);
        self.banner = Some(Banner::Info(
            format!("已从 {} 导入当前 IP / 掩码 / 网关 / DNS", adapter.friendly_name).into(),
        ));
        cx.notify();
    }

    fn reset_from_saved(&mut self, cx: &mut Context<Self>) {
        if let Ok(reloaded) = load_config(&self.config_path) {
            self.saved_config = reloaded;
        }
        self.draft_config = self.saved_config.clone();
        self.show_notifications = self.draft_config.ui.show_notifications;
        self.left_click_toggle = self.draft_config.ui.left_click_toggle;
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
                    "全部配置已保存至 config.json，并已同步到系统托盘".into(),
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
        if self.switching_profile_name.is_some() {
            return;
        }
        if !self.is_admin {
            self.banner = Some(Banner::Error(
                "当前不是管理员权限，无法修改网卡配置；请以管理员身份运行或从托盘打开".into(),
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

        let Some(selected_id) = self.selected_profile_id.clone() else {
            self.banner = Some(Banner::Error("请先在中间栏选择或新建一套 Profile".into()));
            cx.notify();
            return;
        };

        let Some(profile) = self
            .draft_config
            .profiles
            .iter()
            .find(|p| p.id == selected_id)
            .cloned()
        else {
            self.banner = Some(Banner::Error("未找到选中的 Profile".into()));
            cx.notify();
            return;
        };

        if let Err(e) = validate_profile(&profile) {
            self.banner = Some(Banner::Error(e.into()));
            cx.notify();
            return;
        }

        let all_profiles = self.draft_config.profiles.clone();
        let target_name = profile.name.clone();
        let target_mode_label = profile.mode.short_label();
        self.switching_profile_name = Some(target_name.clone());
        self.banner = Some(Banner::Info(
            format!(
                "正在将 {} 切换到「{}」({})...",
                profile.interface_name, target_name, target_mode_label
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
                                "已应用「{}」（{}）",
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
        let ip_text = adapter.ipv4.as_deref().unwrap_or("无 IP");
        let profile_count = self
            .draft_config
            .profiles_for_interface(&adapter.friendly_name)
            .len();

        let (dot_color, state_label, state_bg) = if adapter.is_up {
            (rgb(0x10b981), "在线", rgb(0x064e3b))
        } else {
            (rgb(0x64748b), "断开", rgb(0x334155))
        };

        div()
            .id(SharedString::from(format!("adapter-{}", adapter.friendly_name)))
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().size(px(8.)).rounded_full().bg(dot_color))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xf8fafc))
                                    .child(adapter.friendly_name.clone()),
                            ),
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
                    .text_size(px(11.))
                    .text_color(rgb(0x94a3b8))
                    .overflow_hidden()
                    .child(adapter.description.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0xcbd5e1))
                    .child(format!(
                        "{} · {}",
                        ip_text,
                        if adapter.dhcp_enabled { "DHCP" } else { "静态" }
                    )),
            )
            .into_any_element()
    }

    fn render_profile_card(&self, profile: &NetworkProfile, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_profile_id.as_deref() == Some(profile.id.as_str());
        let active_now = self.current_status.matched_profile_id.as_deref() == Some(profile.id.as_str());
        let id_for_click = profile.id.clone();

        let (mode_label, mode_bg) = if profile.mode.is_dhcp() {
            ("DHCP", rgb(0x065f46))
        } else {
            ("静态 IP", rgb(0x1d4ed8))
        };

        div()
            .id(SharedString::from(format!("profile-{}", profile.id)))
            .flex()
            .flex_col()
            .gap(px(5.))
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
                    this.select_profile(id_for_click.clone(), cx);
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .child(
                        div()
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
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(mode_bg)
                            .text_size(px(10.))
                            .text_color(rgb(0xf8fafc))
                            .child(mode_label),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0x94a3b8))
                    .child(profile.mode.summary()),
            )
            .when(active_now, |this| {
                this.child(
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
                        ),
                )
            })
            .into_any_element()
    }

    fn render_field(label: &str, input: Entity<TextInput>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(5.))
            .flex_1()
            .child(
                div()
                    .text_size(px(12.))
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
            .flex()
            .items_center()
            .justify_between()
            .py(px(4.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(on_toggle))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(0xe2e8f0))
                    .child(label.to_string()),
            )
            .child(
                div()
                    .w(px(38.))
                    .h(px(20.))
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

    fn render_editor_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(_) = self.selected_profile_id.as_ref() else {
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
                                .child("+ 新建静态 IP Profile"),
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
        };

        let mode_section: AnyElement = if self.editor_is_dhcp {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(12.))
                .rounded(px(8.))
                .bg(rgb(0x0f172a))
                .border_1()
                .border_color(rgb(0x1e293b))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(0x94a3b8))
                        .child("DHCP 模式会将所选网卡的 IPv4 地址、默认网关和 DNS 服务器全部设为自动获取。"),
                )
                .child(Self::render_toggle_row(
                    "toggle-profile-renew",
                    "切换后自动执行租约刷新 (ipconfig /renew)",
                    self.editor_renew_on_switch,
                    |this, _, _, cx| {
                        this.editor_renew_on_switch = !this.editor_renew_on_switch;
                        let _ = this.sync_editor_into_draft(cx);
                        cx.notify();
                    },
                    cx,
                ))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field("静态 IPv4 地址", self.ip_input.clone()))
                        .child(Self::render_field("子网掩码", self.mask_input.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(Self::render_field(
                            "默认网关 (如旁路由 IP)",
                            self.gateway_input.clone(),
                        ))
                        .child(Self::render_field(
                            "网关跃点数 (0=自动)",
                            self.metric_input.clone(),
                        )),
                )
                .child(Self::render_field(
                    "DNS 服务器（多条请用英文逗号分隔）",
                    self.dns_input.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .id("btn-import-current")
                                .px(px(10.))
                                .py(px(5.))
                                .rounded(px(6.))
                                .bg(rgb(0x1e293b))
                                .border_1()
                                .border_color(rgb(0x334155))
                                .text_size(px(11.))
                                .text_color(rgb(0x93c5fd))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(0x334155)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.import_selected_adapter_info(cx);
                                    }),
                                )
                                .child("从当前网卡导入 IP / 网关 / DNS"),
                        ),
                )
                .into_any_element()
        };

        div()
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
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xe2e8f0))
                            .child("Profile 详细配置"),
                    )
                    .child(
                        div()
                            .flex()
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
            // Mode Segmented Control
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0x94a3b8))
                            .child("网络模式"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("mode-btn-static")
                                    .flex_1()
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .text_center()
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .cursor_pointer()
                                    .bg(if !self.editor_is_dhcp {
                                        rgb(0x2563eb)
                                    } else {
                                        rgb(0x0f172a)
                                    })
                                    .border_1()
                                    .border_color(if !self.editor_is_dhcp {
                                        rgb(0x60a5fa)
                                    } else {
                                        rgb(0x334155)
                                    })
                                    .text_color(rgb(0xf8fafc))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.set_editor_mode(false, cx);
                                        }),
                                    )
                                    .child("静态 IP / 固定网关"),
                            )
                            .child(
                                div()
                                    .id("mode-btn-dhcp")
                                    .flex_1()
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .text_center()
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .cursor_pointer()
                                    .bg(if self.editor_is_dhcp {
                                        rgb(0x059669)
                                    } else {
                                        rgb(0x0f172a)
                                    })
                                    .border_1()
                                    .border_color(if self.editor_is_dhcp {
                                        rgb(0x34d399)
                                    } else {
                                        rgb(0x334155)
                                    })
                                    .text_color(rgb(0xf8fafc))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.set_editor_mode(true, cx);
                                        }),
                                    )
                                    .child("DHCP 自动获取"),
                            ),
                    ),
            )
            .child(mode_section)
            .into_any_element()
    }
}

fn extract_profile_editor_fields(
    profile: Option<&NetworkProfile>,
) -> (String, bool, bool, String, String, String, String, String) {
    let Some(p) = profile else {
        return (
            String::new(),
            false,
            true,
            String::new(),
            "255.255.255.0".to_string(),
            String::new(),
            "0".to_string(),
            String::new(),
        );
    };
    match &p.mode {
        ProfileMode::Dhcp { renew_on_switch } => (
            p.name.clone(),
            true,
            *renew_on_switch,
            "192.168.1.100".to_string(),
            "255.255.255.0".to_string(),
            "192.168.1.2".to_string(),
            "0".to_string(),
            "192.168.1.1".to_string(),
        ),
        ProfileMode::Static {
            ip,
            subnet_mask,
            gateway,
            gateway_metric,
            dns,
        } => (
            p.name.clone(),
            false,
            true,
            ip.clone(),
            subnet_mask.clone(),
            gateway.clone(),
            gateway_metric.to_string(),
            dns.join(", "),
        ),
    }
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
        let toggle_left_click = Self::render_toggle_row(
            "toggle-left-click",
            "左键单击托盘图标在当前网卡 Profile 间循环切换",
            self.left_click_toggle,
            |this, _, _, cx| {
                this.left_click_toggle = !this.left_click_toggle;
                cx.notify();
            },
            cx,
        );
        let interval_field = Self::render_field(
            "后台轮询间隔（秒，最小 3）",
            self.interval_input.clone(),
        );

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
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(18.))
                    .py(px(13.))
                    .bg(rgb(0x111827))
                    .border_b_1()
                    .border_color(rgb(0x1e293b))
                    .child(
                        div()
                            .flex()
                            .flex_col()
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
                                                "管理员权限"
                                            } else {
                                                "非管理员"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0x94a3b8))
                                    .child(format!(
                                        "配置文件: {}  ·  关闭窗口后程序继续常驻右下角托盘",
                                        self.config_path.display()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("btn-refresh-adapters")
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
            // Body (Three Columns: Adapters | Profiles | Editor)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(12.))
                    .p(px(14.))
                    // Column 1: Network Adapters
                    .child(
                        div()
                            .w(px(245.))
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
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .gap(px(7.))
                                    .overflow_y_scroll()
                                    .children(adapter_cards),
                            ),
                    )
                    // Column 2: Profiles for Selected Adapter
                    .child(
                        div()
                            .w(px(240.))
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
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h(px(0.))
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
                                            .child("+ 静态 IP"),
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
                                            .child("+ DHCP"),
                                    ),
                            ),
                    )
                    // Column 3: Live Status + Selected Profile Editor + Global Tray Options
                    .child(
                        div()
                            .id("right-config-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(10.))
                            .overflow_y_scroll()
                            // Live Status Card
                            .child(
                                div()
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
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(8.))
                                                    .child(
                                                        div()
                                                            .text_size(px(13.))
                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                            .child(format!("网卡实时状态: {}", active_iface)),
                                                    )
                                                    .child(
                                                        div()
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
                                    .child(toggle_left_click)
                                    .child(div().w(px(200.)).pt(px(2.)).child(interval_field)),
                            ),
                    ),
            )
            // Footer Action Bar
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(18.))
                    .py(px(12.))
                    .bg(rgb(0x111827))
                    .border_t_1()
                    .border_color(rgb(0x1e293b))
                    .child(div().flex_1().text_size(px(12.)).child(
                        match &self.banner {
                            Some(Banner::Info(msg)) => {
                                div().text_color(rgb(0x93c5fd)).child(msg.clone())
                            }
                            Some(Banner::Success(msg)) => {
                                div().text_color(rgb(0x34d399)).child(msg.clone())
                            }
                            Some(Banner::Error(msg)) => {
                                div().text_color(rgb(0xf87171)).child(msg.clone())
                            }
                            None => div()
                                .text_color(rgb(0x64748b))
                                .child("支持为每张网卡创建多套静态 IP / DHCP Profile，保存后立即同步到托盘菜单"),
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("btn-reset")
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
                                    .child("保存配置"),
                            )
                            .child(
                                div()
                                    .id("btn-save-and-apply")
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

pub fn run(config: Config, config_path: PathBuf) {
    Application::new().run(move |cx: &mut App| {
        bind_text_input_keys(cx);
        cx.bind_keys([KeyBinding::new("ctrl-w", CloseWindow, None)]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1020.0), px(680.0)), cx);
        let cfg_clone = config.clone();
        let path_clone = config_path.clone();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(GUI_WINDOW_TITLE.into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(860.0), px(560.0))),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let view = ConfigWindow::new(cx, cfg_clone, path_clone);
                    window.focus(&view.focus_handle);
                    view
                })
            },
        )
        .expect("failed to open GPUI config window");

        cx.activate(true);
    });
}
