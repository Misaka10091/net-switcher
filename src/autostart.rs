use std::fs;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const TASK_NAME: &str = "NetSwitcher";

pub fn is_enabled() -> bool {
    Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn set_enabled(enable: bool) -> Result<(), String> {
    if !enable {
        let out = Command::new("schtasks.exe")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("执行 schtasks 失败: {}", e))?;
        if !out.status.success() && is_enabled() {
            return Err("删除开机自启计划任务失败，请确认当前已以管理员身份运行".to_string());
        }
        return Ok(());
    }

    let exe_path = std::env::current_exe().map_err(|e| format!("无法获取当前程序路径: {}", e))?;
    let work_dir = exe_path
        .parent()
        .ok_or_else(|| "无法获取当前程序所在目录".to_string())?;

    let xml = build_task_xml(&exe_path.to_string_lossy(), &work_dir.to_string_lossy());
    let temp_path = std::env::temp_dir().join(format!("netswitcher_task_{}.xml", std::process::id()));

    // Task Scheduler XML expects UTF-16 LE with BOM when declared encoding="UTF-16"
    let mut utf16_bytes = Vec::with_capacity(2 + xml.len() * 2);
    utf16_bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in xml.encode_utf16() {
        utf16_bytes.extend_from_slice(&unit.to_le_bytes());
    }

    fs::write(&temp_path, utf16_bytes)
        .map_err(|e| format!("写入临时任务计划 XML 失败: {}", e))?;

    let out = Command::new("schtasks.exe")
        .args([
            "/Create",
            "/TN",
            TASK_NAME,
            "/XML",
            &temp_path.to_string_lossy(),
            "/F",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let _ = fs::remove_file(&temp_path);

    let out = out.map_err(|e| format!("执行 schtasks 创建任务失败: {}", e))?;
    if !out.status.success() {
        let stderr = crate::winutil::decode_system_bytes(&out.stderr);
        let stdout = crate::winutil::decode_system_bytes(&out.stdout);
        return Err(format!(
            "创建开机自启计划任务失败 (code={:?}): {} {}",
            out.status.code(),
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(())
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn build_task_xml(exe_path: &str, work_dir: &str) -> String {
    let exe_escaped = xml_escape(exe_path);
    let dir_escaped = xml_escape(work_dir);

    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Net Switcher - Windows 网络模式托盘切换工具开机自启</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe_escaped}</Command>
      <Arguments>--minimized</Arguments>
      <WorkingDirectory>{dir_escaped}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}
