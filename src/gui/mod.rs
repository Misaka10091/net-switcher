mod text_input;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*,
    px, rgb, size,
};

use crate::config::{Config, DhcpConfig, StaticConfig, UiConfig, load_config, save_config};
use crate::net::{
    AdapterSummary, NetworkMode, NetworkStatus, SwitchTarget, list_adapters, query_status,
    switch_mode,
};
use crate::tray::{TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG};
use crate::winutil::{is_elevated, notify_tray_reload};

use text_input::{TextInput, bind_text_input_keys};

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
    selected_interface: String,
    adapters: Vec<AdapterSummary>,
    current_status: NetworkStatus,
    is_admin: bool,
    switching: bool,
    banner: Option<Banner>,

    ip_input: Entity<TextInput>,
    mask_input: Entity<TextInput>,
    gateway_input: Entity<TextInput>,
    metric_input: Entity<TextInput>,
    dns_input: Entity<TextInput>,
    interval_input: Entity<TextInput>,

    renew_on_switch: bool,
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
        let current_status = query_status(&config);
        let dns_joined = config.static_cfg.dns.join(", ");

        let ip_input = cx.new(|cx| TextInput::new(cx, config.static_cfg.ip.clone(), "如 192.168.1.100"));
        let mask_input =
            cx.new(|cx| TextInput::new(cx, config.static_cfg.subnet_mask.clone(), "如 255.255.255.0"));
        let gateway_input =
            cx.new(|cx| TextInput::new(cx, config.static_cfg.gateway.clone(), "如 192.168.1.2"));
        let metric_input = cx.new(|cx| {
            TextInput::new(cx, config.static_cfg.gateway_metric.to_string(), "0 表示自动跃点")
        });
        let dns_input =
            cx.new(|cx| TextInput::new(cx, dns_joined, "如 192.168.1.1, 223.5.5.5"));
        let interval_input = cx.new(|cx| {
            TextInput::new(cx, config.ui.refresh_interval_seconds.to_string(), "默认 15")
        });

        let selected_interface = config.interface_name.clone();
        let renew_on_switch = config.dhcp.renew_on_switch;
        let show_notifications = config.ui.show_notifications;
        let left_click_toggle = config.ui.left_click_toggle;

        Self {
            focus_handle: cx.focus_handle(),
            config_path,
            saved_config: config,
            selected_interface,
            adapters,
            current_status,
            is_admin: is_elevated(),
            switching: false,
            banner: None,
            ip_input,
            mask_input,
            gateway_input,
            metric_input,
            dns_input,
            interval_input,
            renew_on_switch,
            show_notifications,
            left_click_toggle,
        }
    }

    fn build_draft_config(&self, cx: &App) -> Result<Config, String> {
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

        let interval_str = self.interval_input.read(cx).text().trim().to_string();
        let refresh_interval_seconds = if interval_str.is_empty() {
            15
        } else {
            interval_str
                .parse::<u32>()
                .map_err(|_| format!("轮询间隔必须是正整数: {}", interval_str))?
                .max(3)
        };

        let cfg = Config {
            interface_name: self.selected_interface.trim().to_string(),
            static_cfg: StaticConfig {
                ip,
                subnet_mask,
                gateway,
                gateway_metric,
                dns,
            },
            dhcp: DhcpConfig {
                renew_on_switch: self.renew_on_switch,
            },
            ui: UiConfig {
                show_notifications: self.show_notifications,
                refresh_interval_seconds,
                left_click_toggle: self.left_click_toggle,
            },
        };
        crate::config::validate_config(&cfg)?;
        Ok(cfg)
    }

    fn refresh_adapters(&mut self, cx: &mut Context<Self>) {
        self.adapters = list_adapters();
        let probe = Config {
            interface_name: self.selected_interface.clone(),
            ..self.saved_config.clone()
        };
        self.current_status = query_status(&probe);
        self.banner = Some(Banner::Info(
            format!("已刷新网卡列表，共检测到 {} 个网络适配器", self.adapters.len()).into(),
        ));
        cx.notify();
    }

    fn select_adapter(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected_interface = name;
        let probe = Config {
            interface_name: self.selected_interface.clone(),
            ..self.saved_config.clone()
        };
        self.current_status = query_status(&probe);
        self.banner = Some(Banner::Info(
            format!("已选择网卡: {}（点击保存以生效）", self.selected_interface).into(),
        ));
        cx.notify();
    }

    fn import_selected_adapter_info(&mut self, cx: &mut Context<Self>) {
        let Some(adapter) = self
            .adapters
            .iter()
            .find(|a| a.friendly_name.eq_ignore_ascii_case(&self.selected_interface))
            .cloned()
        else {
            self.banner = Some(Banner::Error("当前选中的网卡不在系统列表中".into()));
            cx.notify();
            return;
        };

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

        self.banner = Some(Banner::Info(
            format!("已从 {} 导入当前 IP / 掩码 / 网关 / DNS", adapter.friendly_name).into(),
        ));
        cx.notify();
    }

    fn reset_from_saved(&mut self, cx: &mut Context<Self>) {
        if let Ok(reloaded) = load_config(&self.config_path) {
            self.saved_config = reloaded;
        }
        let cfg = self.saved_config.clone();
        self.selected_interface = cfg.interface_name.clone();
        self.ip_input
            .update(cx, |i, cx| i.set_text(cfg.static_cfg.ip.clone(), cx));
        self.mask_input.update(cx, |i, cx| {
            i.set_text(cfg.static_cfg.subnet_mask.clone(), cx)
        });
        self.gateway_input
            .update(cx, |i, cx| i.set_text(cfg.static_cfg.gateway.clone(), cx));
        self.metric_input.update(cx, |i, cx| {
            i.set_text(cfg.static_cfg.gateway_metric.to_string(), cx)
        });
        self.dns_input
            .update(cx, |i, cx| i.set_text(cfg.static_cfg.dns.join(", "), cx));
        self.interval_input.update(cx, |i, cx| {
            i.set_text(cfg.ui.refresh_interval_seconds.to_string(), cx)
        });
        self.renew_on_switch = cfg.dhcp.renew_on_switch;
        self.show_notifications = cfg.ui.show_notifications;
        self.left_click_toggle = cfg.ui.left_click_toggle;
        self.current_status = query_status(&cfg);
        self.banner = Some(Banner::Info("已重置为磁盘中保存的配置".into()));
        cx.notify();
    }

    fn save_only(&mut self, cx: &mut Context<Self>) -> Option<Config> {
        match self.build_draft_config(cx) {
            Ok(cfg) => match save_config(&self.config_path, &cfg) {
                Ok(()) => {
                    self.saved_config = cfg.clone();
                    self.current_status = query_status(&cfg);
                    notify_tray_reload(TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG);
                    self.banner = Some(Banner::Success(
                        "配置已保存至 config.json，并已通知托盘热重载".into(),
                    ));
                    cx.notify();
                    Some(cfg)
                }
                Err(e) => {
                    self.banner = Some(Banner::Error(e.into()));
                    cx.notify();
                    None
                }
            },
            Err(e) => {
                self.banner = Some(Banner::Error(e.into()));
                cx.notify();
                None
            }
        }
    }

    fn trigger_switch(&mut self, target: SwitchTarget, save_first: bool, cx: &mut Context<Self>) {
        if self.switching {
            return;
        }
        if !self.is_admin {
            self.banner = Some(Banner::Error(
                "当前不是管理员权限，无法修改网卡配置；请从系统托盘打开配置界面或以管理员身份运行"
                    .into(),
            ));
            cx.notify();
            return;
        }

        let cfg = if save_first {
            let Some(c) = self.save_only(cx) else {
                return;
            };
            c
        } else {
            match self.build_draft_config(cx) {
                Ok(c) => c,
                Err(e) => {
                    self.banner = Some(Banner::Error(e.into()));
                    cx.notify();
                    return;
                }
            }
        };

        self.switching = true;
        self.banner = Some(Banner::Info(
            format!("正在切换 {} 到 {}...", cfg.interface_name, target.display_name()).into(),
        ));
        cx.notify();

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { switch_mode(&cfg, target) })
                .await;

            let _ = view.update(cx, |this, cx| {
                this.switching = false;
                this.adapters = list_adapters();
                match result {
                    Ok(status) => {
                        this.current_status = status;
                        notify_tray_reload(TRAY_WINDOW_CLASS, WM_APP_RELOAD_CONFIG);
                        this.banner = Some(Banner::Success(
                            format!(
                                "已切换到 {}（{}）",
                                target.display_name(),
                                this.current_status.summary_line()
                            )
                            .into(),
                        ));
                    }
                    Err(err) => {
                        let probe = Config {
                            interface_name: this.selected_interface.clone(),
                            ..this.saved_config.clone()
                        };
                        this.current_status = query_status(&probe);
                        this.banner = Some(Banner::Error(err.into()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_adapter_card(&self, adapter: &AdapterSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = adapter
            .friendly_name
            .eq_ignore_ascii_case(&self.selected_interface);
        let name_for_click = adapter.friendly_name.clone();
        let ip_text = adapter.ipv4.as_deref().unwrap_or("无 IP");
        let gw_text = adapter.gateway.as_deref().unwrap_or("-");

        let (dot_color, state_label, state_bg) = if adapter.is_up {
            (rgb(0x10b981), "已连接", rgb(0x064e3b))
        } else {
            (rgb(0x64748b), "未连接", rgb(0x334155))
        };

        let mode_badge = if adapter.dhcp_enabled {
            ("DHCP", rgb(0x065f46))
        } else {
            ("静态", rgb(0x1e3a8a))
        };

        div()
            .id(SharedString::from(format!("adapter-{}", adapter.friendly_name)))
            .flex()
            .flex_col()
            .gap(px(5.))
            .p(px(10.))
            .rounded(px(8.))
            .bg(if selected { rgb(0x172554) } else { rgb(0x0f172a) })
            .border_1()
            .border_color(if selected { rgb(0x3b82f6) } else { rgb(0x334155) })
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
                            .gap(px(7.))
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
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(state_bg)
                                    .text_size(px(10.))
                                    .text_color(rgb(0xe2e8f0))
                                    .child(state_label),
                            )
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(mode_badge.1)
                                    .text_size(px(10.))
                                    .text_color(rgb(0xe2e8f0))
                                    .child(mode_badge.0),
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
                    .child(format!("IP: {}  ·  GW: {}", ip_text, gw_text)),
            )
    }

    fn render_field(label: &str, input: Entity<TextInput>) -> impl IntoElement {
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
    }

    fn render_toggle_row(
        id: &'static str,
        label: &str,
        checked: bool,
        on_toggle: impl Fn(&mut Self, &gpui::MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .text_size(px(13.))
                    .text_color(rgb(0xe2e8f0))
                    .child(label.to_string()),
            )
            .child(
                div()
                    .w(px(40.))
                    .h(px(22.))
                    .rounded_full()
                    .p(px(2.))
                    .flex()
                    .items_center()
                    .bg(if checked { rgb(0x2563eb) } else { rgb(0x334155) })
                    .justify_end()
                    .when(!checked, |this| this.justify_start())
                    .child(div().size(px(18.)).rounded_full().bg(rgb(0xffffff))),
            )
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

        let switch_disabled = self.switching;
        let adapter_cards: Vec<_> = self
            .adapters
            .iter()
            .map(|a| self.render_adapter_card(a, cx))
            .collect();

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
                    .py(px(14.))
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
                                            .child("Net Switcher · 网卡与网络配置"),
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
                                                "只读 / 非管理员"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(0x94a3b8))
                                    .child(format!("配置文件: {}", self.config_path.display())),
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
            // Body (two columns)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(16.))
                    .p(px(16.))
                    // Left: Adapter list
                    .child(
                        div()
                            .w(px(290.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
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
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xe2e8f0))
                                            .child("1. 选择目标网卡"),
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
                                    .gap(px(8.))
                                    .overflow_y_scroll()
                                    .children(adapter_cards),
                            )
                            .child(
                                div()
                                    .id("btn-import-current")
                                    .w_full()
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(rgb(0x1e293b))
                                    .border_1()
                                    .border_color(rgb(0x334155))
                                    .text_center()
                                    .text_size(px(12.))
                                    .text_color(rgb(0x93c5fd))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x334155)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.import_selected_adapter_info(cx);
                                        }),
                                    )
                                    .child("导入该网卡当前 IP / 网关 / DNS"),
                            ),
                    )
                    // Right: Live status + Form
                    .child(
                        div()
                            .id("right-config-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(12.))
                            .overflow_y_scroll()
                            // Live Status Card
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
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
                                                            .child(format!(
                                                                "当前目标: {}",
                                                                self.selected_interface
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .px(px(8.))
                                                            .py(px(2.))
                                                            .rounded(px(4.))
                                                            .bg(mode_color)
                                                            .text_size(px(11.))
                                                            .text_color(rgb(0xffffff))
                                                            .child(
                                                                if self.switching {
                                                                    "正在切换..."
                                                                } else {
                                                                    self.current_status
                                                                        .mode
                                                                        .display_name()
                                                                },
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .gap(px(8.))
                                                    .child(
                                                        div()
                                                            .id("btn-quick-static")
                                                            .px(px(10.))
                                                            .py(px(5.))
                                                            .rounded(px(6.))
                                                            .bg(if switch_disabled {
                                                                rgb(0x334155)
                                                            } else {
                                                                rgb(0x2563eb)
                                                            })
                                                            .text_size(px(12.))
                                                            .text_color(rgb(0xffffff))
                                                            .cursor_pointer()
                                                            .hover(|s| s.bg(rgb(0x1d4ed8)))
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(|this, _, _, cx| {
                                                                    this.trigger_switch(
                                                                        SwitchTarget::Static,
                                                                        true,
                                                                        cx,
                                                                    );
                                                                }),
                                                            )
                                                            .child("切到固定网关"),
                                                    )
                                                    .child(
                                                        div()
                                                            .id("btn-quick-dhcp")
                                                            .px(px(10.))
                                                            .py(px(5.))
                                                            .rounded(px(6.))
                                                            .bg(if switch_disabled {
                                                                rgb(0x334155)
                                                            } else {
                                                                rgb(0x059669)
                                                            })
                                                            .text_size(px(12.))
                                                            .text_color(rgb(0xffffff))
                                                            .cursor_pointer()
                                                            .hover(|s| s.bg(rgb(0x047857)))
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(|this, _, _, cx| {
                                                                    this.trigger_switch(
                                                                        SwitchTarget::Dhcp,
                                                                        false,
                                                                        cx,
                                                                    );
                                                                }),
                                                            )
                                                            .child("切到 DHCP 自动"),
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
                            // Static Config Form Card
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .p(px(14.))
                                    .rounded(px(10.))
                                    .bg(rgb(0x111827))
                                    .border_1()
                                    .border_color(rgb(0x1e293b))
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xe2e8f0))
                                            .child("2. 固定网关静态配置"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(12.))
                                            .child(Self::render_field(
                                                "静态 IPv4 地址",
                                                self.ip_input.clone(),
                                            ))
                                            .child(Self::render_field(
                                                "子网掩码",
                                                self.mask_input.clone(),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap(px(12.))
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
                                        "DNS 服务器（支持多条，用英文逗号分隔）",
                                        self.dns_input.clone(),
                                    )),
                            )
                            // Behavior & UI Options Card
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .p(px(14.))
                                    .rounded(px(10.))
                                    .bg(rgb(0x111827))
                                    .border_1()
                                    .border_color(rgb(0x1e293b))
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xe2e8f0))
                                            .child("3. DHCP 与托盘行为"),
                                    )
                                    .child(Self::render_toggle_row(
                                        "toggle-renew",
                                        "切换到 DHCP 后自动刷新租约 (ipconfig /renew)",
                                        self.renew_on_switch,
                                        |this, _, _, cx| {
                                            this.renew_on_switch = !this.renew_on_switch;
                                            cx.notify();
                                        },
                                        cx,
                                    ))
                                    .child(Self::render_toggle_row(
                                        "toggle-notify",
                                        "切换完成时弹出右下角气泡通知",
                                        self.show_notifications,
                                        |this, _, _, cx| {
                                            this.show_notifications = !this.show_notifications;
                                            cx.notify();
                                        },
                                        cx,
                                    ))
                                    .child(Self::render_toggle_row(
                                        "toggle-left-click",
                                        "左键单击托盘图标一键切换模式",
                                        self.left_click_toggle,
                                        |this, _, _, cx| {
                                            this.left_click_toggle = !this.left_click_toggle;
                                            cx.notify();
                                        },
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .w(px(200.))
                                            .pt(px(4.))
                                            .child(Self::render_field(
                                                "状态轮询间隔（秒，最小 3）",
                                                self.interval_input.clone(),
                                            )),
                                    ),
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
                            Some(Banner::Info(msg)) => div()
                                .text_color(rgb(0x93c5fd))
                                .child(msg.clone()),
                            Some(Banner::Success(msg)) => div()
                                .text_color(rgb(0x34d399))
                                .child(msg.clone()),
                            Some(Banner::Error(msg)) => div()
                                .text_color(rgb(0xf87171))
                                .child(msg.clone()),
                            None => div()
                                .text_color(rgb(0x64748b))
                                .child("修改配置后点击保存即可自动同步到托盘程序"),
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
                                    .id("btn-save")
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
                                            this.save_only(cx);
                                        }),
                                    )
                                    .child("保存配置"),
                            )
                            .child(
                                div()
                                    .id("btn-save-apply")
                                    .px(px(14.))
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(if switch_disabled {
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
                                            this.trigger_switch(SwitchTarget::Static, true, cx);
                                        }),
                                    )
                                    .child("保存并应用固定网关"),
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

        let bounds = Bounds::centered(None, size(px(880.0), px(620.0)), cx);
        let cfg_clone = config.clone();
        let path_clone = config_path.clone();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Net Switcher — 网卡与网络配置".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(760.0), px(540.0))),
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
