# Net Switcher

轻量级 Windows 单进程系统托盘 + **GPUI** 多网卡 / 多 Profile 网络配置切换工具（Rust 原生 Win32 + GPUI 实现）。

## 特性

- **多网卡 × 多 Profile 管理**：
  - 自动枚举本机所有物理/无线网卡，显示在线状态、当前 IPv4 与 IPv6 状态。
  - 每张网卡下可创建任意多套命名 Profile（如「旁路由网关」「双栈静态」「DHCP 自动」等），支持新建、复制、删除、一键导入当前网卡配置。
- **同一套 Profile 内独立配置 IPv4 & IPv6**：
  - **IPv4** 可选：`静态 IP` / `DHCP 自动` / `关闭 IPv4 (ms_tcpip)`。
  - **IPv6** 可选：`DHCP 自动` / `静态 IPv6` / `关闭 IPv6 (ms_tcpip6)`。
  - 所有配置项编辑时仅暂存于 Profile，**统一在最终点击「切换 / 应用」时一次性对网卡生效**，避免误触直接改网。
- **严格单进程互斥 + 启动即弹 GPUI 配置窗**：
  - 托盘与 GPUI 配置窗口运行在**同一个进程**中，全系统通过跨权限互斥锁（处理 `ERROR_ALREADY_EXISTS` 与 `ERROR_ACCESS_DENIED`）严格只允许启动一个实例。
  - 双击 `net-switcher.exe`：一次 UAC 提权后常驻托盘并**立刻弹出 GPUI 配置窗口**；关闭窗口后留在系统托盘常驻。
  - 重复双击 `net-switcher.exe`：不启动新进程，直接唤醒并前置已有唯一实例的配置窗口（免二次 UAC）。
  - 开机自启动：通过 Windows 任务计划带 `--minimized` 静默启动至托盘，免 UAC 且不弹窗打扰。
- **高效交互与兼容性**：
  - 配置窗口中栏每张 Profile 卡片均提供一键 **`切换`** 按钮，顶部状态栏提供 **`一键切换下一套`** 快捷按钮。
  - 系统托盘左键单击可在当前网卡的多套 Profile 间顺次循环切换；右键菜单列出所有 Profile，生效项带勾选，点击直达。
  - 自动无损兼容并迁移旧版 v1 / v2 `config.json` 配置。

## 命令行参数

```powershell
# 仅静默启动到系统托盘（开机自启默认使用）
.\net-switcher.exe --minimized

# 只读诊断当前网卡、IPv4/IPv6 与 Profile 状态（免 UAC）
.\net-switcher.exe --status
```

## 配置文件示例 (`config.json`, v3)

```json
{
  "version": 3,
  "activeInterface": "WLAN",
  "profiles": [
    {
      "id": "wlan-static-gateway",
      "name": "旁路由固定网关",
      "interfaceName": "WLAN",
      "ipv4": {
        "type": "static",
        "ip": "192.168.1.100",
        "subnetMask": "255.255.255.0",
        "gateway": "192.168.1.2",
        "gatewayMetric": 0,
        "dns": [
          "192.168.1.1"
        ]
      },
      "ipv6": {
        "type": "disabled"
      }
    },
    {
      "id": "wlan-dhcp-auto",
      "name": "DHCP 双栈自动",
      "interfaceName": "WLAN",
      "ipv4": {
        "type": "dhcp",
        "renewOnSwitch": true
      },
      "ipv6": {
        "type": "dhcp"
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
