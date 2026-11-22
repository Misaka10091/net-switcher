# Net Switcher

轻量级 Windows 系统托盘 + **GPUI** 多网卡 / 多 Profile 网络配置切换工具（Rust 原生 Win32 + GPUI 实现）。

每张网卡均可配置**多套命名 Profile**（每套 Profile 可自由选择 **静态 IP / 固定网关** 或 **DHCP 自动获取** 模式），启动时先弹出 GPUI 配置窗口，关闭窗口后程序继续以极低资源常驻系统托盘。

## 特性

- **多网卡 × 多 Profile 管理**：
  - 自动枚举本机所有物理/无线网卡，显示在线状态、当前 IP、IPv6 开关与模式。
  - 每张网卡下可创建任意多套 Profile（如「旁路由网关」「公司静态」「DHCP 自动」等），支持新建、复制、删除、一键导入当前网卡配置。
  - **DHCP** 与 **静态 IP** 作为每套 Profile 的两种网络模式，不再限制为全局二选一。
- **独立 IPv6 开关 + Profile 联动**：
  - 支持在 GPUI 实时状态栏和系统托盘右键菜单中一键开启/关闭指定网卡的 IPv6（底层操作网卡 `ms_tcpip6` 绑定，不影响其他网卡）。
  - 每套 Profile 可独立设置切换时的 IPv6 策略：`保持不变` (`keep`)、`关闭 IPv6` (`disable`) 或 `开启 IPv6` (`enable`)，例如切到旁路由时自动关掉 IPv6、切回 DHCP 时自动恢复 IPv6。
- **启动即弹 GPUI 配置界面，关窗常驻托盘**：
  - 双击 `net-switcher.exe`：自动完成一次 UAC 提权，常驻系统托盘并**立刻弹出 GPUI 配置窗口**；关闭窗口后彻底释放 GPU 显存，托盘继续轻量化常驻。
  - 再次双击 `net-switcher.exe`：不重复启动实例，直接唤起/前置 GPUI 配置窗口（免二次 UAC）。
  - 开机自启动：通过 Windows 任务计划以 `--minimized` 静默启动至托盘，免 UAC 且不弹窗打扰。
- **高效托盘交互**：
  - **左键单击图标**：在当前网卡的多套 Profile 之间顺次循环切换。
  - **右键菜单**：显示当前网卡实时状态及命中的 Profile、一键切换下一套、列出所有 Profile（带生效中勾选标记，点击直达）、打开 GPUI 配置界面、刷新/重载、免 UAC 开机自启、退出。
  - **直观图标颜色**：科技蓝（命中静态 Profile）、靛紫色（未匹配的静态 IP）、翠绿色（DHCP 自动）、琥珀橙（切换中）、告警红（未连接/异常）。
- **实时同步与稳定性**：
  - 基于 Win32 `GetAdaptersAddresses` + `NotifyIpInterfaceChange` 内核事件监听 + 定时轮询，不受系统语言影响。
  - 支持 `PerMonitorV2` DPI、单实例互斥锁、Explorer 重启后自动恢复托盘图标、RAII 自动释放 HICON。
  - 自动兼容并迁移旧版 v1 `config.json` 配置。

## 快速使用

1. 前往 GitHub Actions 构建产物或 Releases 下载 `net-switcher.exe`。
2. 直接双击运行 `net-switcher.exe`（可按需先将 `config.example.json` 复制为 `config.json` 放同目录，未放置时也会自动生成默认配置供界面编辑）。
3. 在弹出的 **GPUI 配置窗口** 中选择网卡、配置多套 Profile 并保存或立即应用；关闭窗口后可随时通过右下角系统托盘一键切换。

### 命令行参数

```powershell
# 仅静默启动到系统托盘（开机自启默认使用）
.\net-switcher.exe --minimized

# 单独打开 GPUI 配置界面
.\net-switcher.exe --gui

# 只读诊断当前网卡、Profile 列表与实时状态（免 UAC）
.\net-switcher.exe --status
```

## 配置文件示例 (`config.json`, v2)

```json
{
  "version": 2,
  "activeInterface": "WLAN",
  "profiles": [
    {
      "id": "wlan-static-gateway",
      "name": "旁路由固定网关",
      "interfaceName": "WLAN",
      "mode": {
        "type": "static",
        "ip": "192.168.1.100",
        "subnetMask": "255.255.255.0",
        "gateway": "192.168.1.2",
        "gatewayMetric": 0,
        "dns": [
          "192.168.1.1"
        ]
      }
    },
    {
      "id": "wlan-dhcp-auto",
      "name": "DHCP 自动获取",
      "interfaceName": "WLAN",
      "mode": {
        "type": "dhcp",
        "renewOnSwitch": true
      }
    }
  ],
  "ui": {
    "showNotifications": true,
    "refreshIntervalSeconds": 15,
    "leftClickToggle": true
  }
}
```

- `activeInterface`：托盘默认监控和循环切换的目标网卡名称。
- `profiles`：命名 Profile 列表，每项绑定一个 `interfaceName` 和 `mode`（`type: "static"` 或 `type: "dhcp"`）。
- `ui.leftClickToggle`：为 `true` 时左键单击托盘在当前网卡的 Profile 间循环切换；为 `false` 时左键单击打开 GPUI 配置窗口。

## 从源码构建

### GitHub Actions 自动构建（推荐）

推送到 `main` 分支即可自动在 `windows-latest` (MSVC) 上运行单元测试并生成 `net-switcher.exe` 与压缩包 Artifact；推送 `v*` 标签自动发布 GitHub Release。

### 本地构建（需安装 Visual Studio C++ Build Tools 与 Windows SDK `fxc.exe`）

```powershell
cargo test --locked
cargo build --release --locked
```
