use std::ptr;

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayVisualState {
    Static,
    StaticCustom,
    Dhcp,
    Switching,
    Disconnected,
}

pub struct OwnedIcon {
    handle: HICON,
}

impl OwnedIcon {
    pub fn handle(&self) -> HICON {
        self.handle
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                DestroyIcon(self.handle);
            }
        }
    }
}

pub fn create_state_icon(state: TrayVisualState) -> Result<OwnedIcon, String> {
    const SIZE: usize = 32;
    let pixels = render_icon_bgra(state, SIZE);
    bgra_to_hicon(&pixels, SIZE as i32, SIZE as i32)
}

fn render_icon_bgra(state: TrayVisualState, size: usize) -> Vec<u8> {
    // 4x4 supersampling for crisp anti-aliased edges on high-DPI displays
    const SS: usize = 4;
    let mut buf = vec![0u8; size * size * 4];

    let (bg_r, bg_g, bg_b) = match state {
        TrayVisualState::Static => (37.0f32, 99.0, 235.0),       // 科技蓝 #2563EB
        TrayVisualState::StaticCustom => (124.0, 58.0, 237.0),   // 靛紫色 #7C3AED
        TrayVisualState::Dhcp => (16.0, 185.0, 129.0),           // 翠绿色 #10B981
        TrayVisualState::Switching => (245.0, 158.0, 11.0),      // 琥珀橙 #F59E0B
        TrayVisualState::Disconnected => (220.0, 38.0, 38.0),    // 告警红 #DC2626
    };

    for y in 0..size {
        for x in 0..size {
            let mut acc_r = 0.0f32;
            let mut acc_g = 0.0f32;
            let mut acc_b = 0.0f32;
            let mut acc_a = 0.0f32;

            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32 * 2.0 - 1.0;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32 * 2.0 - 1.0;

                    if let Some((r, g, b, a)) = sample_pixel(state, fx, fy, bg_r, bg_g, bg_b) {
                        acc_r += r * a;
                        acc_g += g * a;
                        acc_b += b * a;
                        acc_a += a;
                    }
                }
            }

            let samples = (SS * SS) as f32;
            let a = (acc_a / samples).clamp(0.0, 1.0);
            let idx = (y * size + x) * 4;
            if a > 0.001 {
                let r = (acc_r / acc_a).clamp(0.0, 255.0);
                let g = (acc_g / acc_a).clamp(0.0, 255.0);
                let b = (acc_b / acc_a).clamp(0.0, 255.0);
                // Windows 32-bit HICON expects BGRA
                buf[idx] = b.round() as u8;
                buf[idx + 1] = g.round() as u8;
                buf[idx + 2] = r.round() as u8;
                buf[idx + 3] = (a * 255.0).round() as u8;
            }
        }
    }

    buf
}

fn sample_pixel(
    state: TrayVisualState,
    x: f32,
    y: f32,
    bg_r: f32,
    bg_g: f32,
    bg_b: f32,
) -> Option<(f32, f32, f32, f32)> {
    // Outer rounded-square badge (squircle-like)
    let badge_d = sdf_rounded_box(x, y, 0.86, 0.86, 0.36);
    if badge_d > 0.0 {
        return None;
    }

    // Subtle dark border on outer rim for contrast on both light & dark taskbars
    let (mut r, mut g, mut b) = if badge_d > -0.07 {
        (bg_r * 0.72, bg_g * 0.72, bg_b * 0.72)
    } else {
        // Gentle top-to-bottom highlight
        let shade = 1.06 - (y + 1.0) * 0.07;
        (
            (bg_r * shade).min(255.0),
            (bg_g * shade).min(255.0),
            (bg_b * shade).min(255.0),
        )
    };

    let fg = match state {
        TrayVisualState::Static | TrayVisualState::StaticCustom => {
            // Static gateway symbol: solid center node + outer static ring + anchor pins
            let dist = (x * x + y * y).sqrt();
            let center_dot = dist <= 0.25;
            let ring = (dist - 0.50).abs() <= 0.085;
            let cross_h = y.abs() <= 0.075 && x.abs() >= 0.45 && x.abs() <= 0.68;
            let cross_v = x.abs() <= 0.075 && y.abs() >= 0.45 && y.abs() <= 0.68;
            center_dot || ring || cross_h || cross_v
        }
        TrayVisualState::Dhcp => {
            // DHCP symbol: Wi-Fi / dynamic broadcast arcs + bottom dot
            let cy = y - 0.42;
            let dist = (x * x + cy * cy).sqrt();
            let angle_ok = cy <= 0.0 && x.abs() <= (-cy) * 1.15;
            let dot = (x * x + (y - 0.38) * (y - 0.38)).sqrt() <= 0.14;
            let arc1 = angle_ok && (dist - 0.42).abs() <= 0.08;
            let arc2 = angle_ok && (dist - 0.74).abs() <= 0.08;
            let arc3 = angle_ok && (dist - 1.06).abs() <= 0.08;
            dot || arc1 || arc2 || arc3
        }
        TrayVisualState::Switching => {
            // Switching symbol: two opposite sync arcs
            let dist = (x * x + y * y).sqrt();
            let on_ring = (dist - 0.48).abs() <= 0.11;
            let gap = (x - y).abs() <= 0.18;
            on_ring && !gap
        }
        TrayVisualState::Disconnected => {
            // Disconnected symbol: bold white X
            let d1 = ((x - y).abs() / 1.4142) <= 0.11;
            let d2 = ((x + y).abs() / 1.4142) <= 0.11;
            let in_bounds = x.abs() <= 0.50 && y.abs() <= 0.50;
            in_bounds && (d1 || d2)
        }
    };

    if fg {
        r = 255.0;
        g = 255.0;
        b = 255.0;
    }

    Some((r, g, b, 1.0))
}

fn sdf_rounded_box(x: f32, y: f32, bx: f32, by: f32, radius: f32) -> f32 {
    let qx = x.abs() - (bx - radius);
    let qy = y.abs() - (by - radius);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - radius
}

fn bgra_to_hicon(bgra: &[u8], width: i32, height: i32) -> Result<OwnedIcon, String> {
    unsafe {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = width;
        bi.bmiHeader.biHeight = -height; // top-down DIB
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;

        let mut bits: *mut std::ffi::c_void = ptr::null_mut();
        let hbm_color = CreateDIBSection(
            ptr::null_mut(),
            &bi,
            DIB_RGB_COLORS,
            &mut bits,
            ptr::null_mut(),
            0,
        );
        if hbm_color.is_null() || bits.is_null() {
            return Err("创建托盘图标 DIB 位图失败".to_string());
        }

        ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), bgra.len());

        // Monochrome mask bitmap (all zeros; alpha channel in hbmColor handles transparency)
        let hbm_mask = CreateBitmap(width, height, 1, 1, ptr::null());
        if hbm_mask.is_null() {
            DeleteObject(hbm_color);
            return Err("创建托盘图标掩码位图失败".to_string());
        }

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };

        let hicon = CreateIconIndirect(&icon_info);
        DeleteObject(hbm_color);
        DeleteObject(hbm_mask);

        if hicon.is_null() {
            return Err("CreateIconIndirect 创建图标失败".to_string());
        }

        Ok(OwnedIcon { handle: hicon })
    }
}
