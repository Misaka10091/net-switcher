# Net Switcher

轻量级 Windows 系统托盘 + **GPUI** 图形配置网络模式切换工具（Rust 原生 Win32 + GPUI 实现），用于在 **固定网关（静态 IP/旁路由）** 与 **DHCP 自动获取** 两种网络配置之间一键快速切换。

## 特性

- **托盘常驻 + 按需 GPUI 界面**：平时仅以原生 Win32 托盘轻量常驻（零 GPU 显存占用），右键点击「网卡与网络配置 (GUI)...」按需拉起硬件加速的 GPUI 图形配置窗口，关闭窗口即彻底回收渲染资源。
- **GPUI 可视化配置**：
  - 自动枚举本机所有物理/无线网卡，实时显示连接状态、DHCP/静态标识、IP 与网关。
  - 支持点击选择目标网卡、一键导入当前网卡的 IP / 掩码 / 网关 / DNS。
  - 可视化编辑静态 IP、子网掩码、默认网关、跃点数、多条 DNS、DHCP 租约刷新及托盘交互开关。
  - 保存后自动写入 `config.json` 并通过窗口消息热通知托盘进程即时生效，也支持在界面内直接切换网络模式。
- **无黑框 & 一次提权**：纯 Windows GUI 子系统，启动时自动检测并通过 UAC 提权一次，后续切换及打开配置窗口零弹窗。
- **直观状态图标**：
  - **蓝色节点**：固定网关模式（与 `config.json` 静态配置一致）
  - **紫色节点**：其他自定义静态配置
  - **绿色信号**：DHCP 自动模式
  - **橙色双弧**：正在切换中
  - **红色叉号**：网卡未找到或已断开
- **实时状态同步**：基于 Win32 `GetAdaptersAddresses` 与 `NotifyIpInterfaceChange` 内核通知 + 定时轮询，不受系统语言影响。
- **免 UAC 开机自启**：右键一键开关，基于 Windows 任务计划程序（登录触发、最高权限、交互式会话、电池不中断、无 72 小时超时限制）。
- **高分屏与稳定性**：支持 `PerMonitorV2` DPI、单实例互斥锁、Explorer 重启后自动恢复托盘图标、RAII 自动释放 HICON。

## 快速使用

1. 前往 GitHub Actions 构建产物或 Releases 下载 `net-switcher.exe`，将 `config.example.json` 复制为 `config.json` 并放在同目录中（或直接运行 `--gui` 通过图形界面选择网卡与配置）。
2. 双击运行 `net-switcher.exe` 并同意 UAC 提权，右下角系统托盘即出现状态图标。
3. **交互操作**：
   - **左键单击图标**：在「固定网关模式」与「DHCP 自动模式」之间一键切换。
   - **鼠标悬停**：查看当前网卡、模式、IP、网关和 DNS。
   - **右键菜单**：查看详情、一键切换、指定切换模式、**打开 GPUI 网卡与网络配置窗口**、立即刷新、打开/重载 `config.json`、开关开机自启、退出。

### 命令行参数

```powershell
# 启动 GPUI 图形配置界面
.\net-switcher.exe --gui

# 只读诊断当前网卡与配置状态（免 UAC）
.\net-switcher.exe --status
```

## 配置文件 (`config.json`)

```json
{
  "interfaceName": "WLAN",
  "static": {
    "ip": "192.168.1.100",
    "subnetMask": "255.255.255.0",
    "gateway": "192.168.1.2",
    "gatewayMetric": 0,
    "dns": [
      "192.168.1.1"
    ]
  },
  "dhcp": {
    "renewOnSwitch": true
  },
  "ui": {
    "showNotifications": true,
    "refreshIntervalSeconds": 15,
    "leftClickToggle": true
  }
}
```

- `interfaceName`：目标网卡名称（与 Windows“网络连接”及 `netsh` 中一致）。
- `static.gatewayMetric`：默认网关跃点数；设为 `0` 表示自动跃点。
- `static.dns`：支持配置一个或多个 DNS（第一条为主 DNS，其余按顺序添加为备用 DNS）。
- `dhcp.renewOnSwitch`：切换到 DHCP 后是否执行 `ipconfig /renew` 刷新租约。
- `ui.showNotifications`：切换完成后是否显示右下角气泡通知。
- `ui.refreshIntervalSeconds`：后台轮询兜底间隔（秒，最小 3 秒）。
- `ui.leftClickToggle`：是否允许左键单击托盘图标直接切换模式。

## 从源码构建

### 使用 Windows GNU 工具链（无需安装 Visual Studio）

```powershell
cargo +stable-x86_64-pc-windows-gnu build --release
Copy-Item .\target\release\net-switcher.exe .\net-switcher.exe -Force
```

### 使用 Windows MSVC 工具链（需已安装 Visual Studio C++ Build Tools）

```powershell
cargo build --release
Copy-Item .\target\release\net-switcher.exe .\net-switcher.exe -Force
```
