use crate::config::{IconStyle, Settings};
use crate::devices::DeviceStatus;
use crate::hidpp::ChargeState;
use crate::winutil::wide;
use anyhow::Result;
use std::collections::HashMap;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ICON_SIZE: i32 = 32;

pub enum UserAction {
    ToggleDevice(String, bool),
    RefreshNow,
    OpenSettings,
    Exit,
}

pub struct TrayManager {
    menu: Menu,
    devices_menu: Submenu,
    device_items: Vec<(String, CheckMenuItem)>,
    no_devices_item: MenuItem,
    refresh_id: MenuId,
    settings_id: MenuId,
    exit_id: MenuId,
    trays: HashMap<String, TrayIcon>,
    /// Shown when no device icon is visible so the app stays reachable.
    fallback: Option<TrayIcon>,
}

impl TrayManager {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let devices_menu = Submenu::new("Devices", true);
        let no_devices_item = MenuItem::new("No Logitech mice found", false, None);
        devices_menu.append(&no_devices_item)?;
        let refresh = MenuItem::new("Refresh now", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let exit = MenuItem::new("Exit", true, None);
        menu.append_items(&[
            &devices_menu,
            &PredefinedMenuItem::separator(),
            &refresh,
            &settings,
            &PredefinedMenuItem::separator(),
            &exit,
        ])?;
        Ok(Self {
            menu,
            devices_menu,
            device_items: Vec::new(),
            no_devices_item,
            refresh_id: refresh.id().clone(),
            settings_id: settings.id().clone(),
            exit_id: exit.id().clone(),
            trays: HashMap::new(),
            fallback: None,
        })
    }

    pub fn handle_menu_event(&self, id: &MenuId) -> Option<UserAction> {
        if *id == self.refresh_id {
            return Some(UserAction::RefreshNow);
        }
        if *id == self.settings_id {
            return Some(UserAction::OpenSettings);
        }
        if *id == self.exit_id {
            return Some(UserAction::Exit);
        }
        for (device_id, item) in &self.device_items {
            if item.id() == id {
                // muda already flipped the checkmark; mirror it into settings.
                return Some(UserAction::ToggleDevice(
                    device_id.clone(),
                    item.is_checked(),
                ));
            }
        }
        None
    }

    pub fn update(&mut self, devices: &[DeviceStatus], settings: &Settings) -> Result<()> {
        self.sync_menu(devices, settings)?;
        self.sync_icons(devices, settings)?;
        Ok(())
    }

    fn sync_menu(&mut self, devices: &[DeviceStatus], settings: &Settings) -> Result<()> {
        let same_set = self.device_items.len() == devices.len()
            && self
                .device_items
                .iter()
                .zip(devices)
                .all(|((id, _), d)| *id == d.id);
        if same_set {
            for ((_, item), device) in self.device_items.iter().zip(devices) {
                item.set_text(menu_label(device));
                item.set_checked(settings.is_selected(&device.id));
            }
            return Ok(());
        }
        for (_, item) in self.device_items.drain(..) {
            let _ = self.devices_menu.remove(&item);
        }
        let _ = self.devices_menu.remove(&self.no_devices_item);
        if devices.is_empty() {
            self.devices_menu.append(&self.no_devices_item)?;
        } else {
            for device in devices {
                let item = CheckMenuItem::new(
                    menu_label(device),
                    true,
                    settings.is_selected(&device.id),
                    None,
                );
                self.devices_menu.append(&item)?;
                self.device_items.push((device.id.clone(), item));
            }
        }
        Ok(())
    }

    fn sync_icons(&mut self, devices: &[DeviceStatus], settings: &Settings) -> Result<()> {
        let shown: Vec<&DeviceStatus> = devices
            .iter()
            .filter(|d| settings.is_selected(&d.id))
            .collect();
        self.trays.retain(|id, _| shown.iter().any(|d| d.id == *id));

        for device in shown {
            let icon = status_icon(device, settings.icon_style);
            let tip = tooltip(device);
            if let Some(tray) = self.trays.get(&device.id) {
                let _ = tray.set_icon(Some(icon));
                let _ = tray.set_tooltip(Some(tip));
            } else {
                let tray = TrayIconBuilder::new()
                    .with_menu(Box::new(self.menu.clone()))
                    .with_icon(icon)
                    .with_tooltip(tip)
                    .with_menu_on_left_click(true)
                    .build()?;
                self.trays.insert(device.id.clone(), tray);
            }
        }

        if self.trays.is_empty() {
            if self.fallback.is_none() {
                let logo = crate::logo::logo_rgba(32);
                let icon = Icon::from_rgba(logo, 32, 32).expect("logo icon");
                let tray = TrayIconBuilder::new()
                    .with_menu(Box::new(self.menu.clone()))
                    .with_icon(icon)
                    .with_tooltip("GPX Battery — no mouse selected")
                    .with_menu_on_left_click(true)
                    .build()?;
                self.fallback = Some(tray);
            }
        } else {
            self.fallback = None;
        }
        Ok(())
    }
}

fn menu_label(device: &DeviceStatus) -> String {
    match (&device.battery, device.online) {
        (Some(b), true) => format!("{} — {}%", device.name, b.percent),
        (Some(b), false) => format!("{} — {}% (asleep)", device.name, b.percent),
        (None, _) => format!("{} — off", device.name),
    }
}

fn tooltip(device: &DeviceStatus) -> String {
    match (&device.battery, device.online) {
        (Some(b), true) => {
            let state = match b.state {
                ChargeState::Charging => " (charging)",
                ChargeState::Full => " (full)",
                ChargeState::Discharging => "",
            };
            format!("{}: {}%{}", device.name, b.percent, state)
        }
        (Some(b), false) => format!(
            "{}: {}% (last known — mouse is asleep or off)",
            device.name, b.percent
        ),
        (None, _) => format!("{}: not responding", device.name),
    }
}

type Rgb = (u8, u8, u8);

const WHITE: Rgb = (0xff, 0xff, 0xff);
const GREEN: Rgb = (0x4c, 0xaf, 0x50);
const AMBER: Rgb = (0xff, 0xc1, 0x07);
const RED: Rgb = (0xf4, 0x43, 0x36);
const GRAY: Rgb = (0x9e, 0x9e, 0x9e);

fn status_color(device: &DeviceStatus) -> Rgb {
    if !device.online {
        // Asleep/off: last known value shown dimmed.
        return GRAY;
    }
    match &device.battery {
        None => GRAY,
        Some(b) => match b.state {
            ChargeState::Charging | ChargeState::Full => GREEN,
            ChargeState::Discharging if b.percent <= 15 => RED,
            ChargeState::Discharging if b.percent <= 30 => AMBER,
            ChargeState::Discharging => WHITE,
        },
    }
}

fn status_icon(device: &DeviceStatus, style: IconStyle) -> Icon {
    let color = status_color(device);
    match style {
        IconStyle::Percentage => {
            let text = match &device.battery {
                Some(b) => b.percent.to_string(),
                None => "?".to_string(),
            };
            text_icon(&text, color)
        }
        IconStyle::BatteryBar => bar_icon(device.battery.as_ref().map(|b| b.percent), color),
    }
}

/// Rasterize `text` with GDI (Segoe UI bold, grayscale-antialiased) and tint
/// it; the glyph coverage becomes the alpha channel.
fn text_icon(text: &str, color: Rgb) -> Icon {
    let size = ICON_SIZE as usize;
    let font_height = if text.len() >= 3 { -19 } else { -30 };
    let alpha = rasterize_text(text, font_height);
    let mut rgba = vec![0u8; size * size * 4];
    for (i, &a) in alpha.iter().enumerate() {
        rgba[i * 4] = color.0;
        rgba[i * 4 + 1] = color.1;
        rgba[i * 4 + 2] = color.2;
        rgba[i * 4 + 3] = a;
    }
    Icon::from_rgba(rgba, size as u32, size as u32).expect("icon from rgba")
}

fn rasterize_text(text: &str, font_height: i32) -> Vec<u8> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
        DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
        FF_DONTCARE, FW_BOLD, GdiFlush, GetDC, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject,
        SetBkMode, SetTextColor, TRANSPARENT,
    };

    let size = ICON_SIZE as usize;
    let mut alpha = vec![0u8; size * size];
    unsafe {
        let screen_dc = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen_dc);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: ICON_SIZE,
            biHeight: -ICON_SIZE, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB as u32,
            ..std::mem::zeroed()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if !bitmap.is_null() && !bits.is_null() {
            let old_bitmap = SelectObject(dc, bitmap as _);
            let face = wide("Segoe UI");
            let font = CreateFontW(
                font_height,
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                // Grayscale AA (not ClearType) so coverage maps cleanly to alpha.
                ANTIALIASED_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            );
            let old_font = SelectObject(dc, font as _);
            SetTextColor(dc, 0x00ff_ffff);
            SetBkMode(dc, TRANSPARENT as i32);
            let text_w = wide(text);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: ICON_SIZE,
                bottom: ICON_SIZE,
            };
            DrawTextW(
                dc,
                text_w.as_ptr(),
                (text_w.len() - 1) as i32,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            GdiFlush();
            // White-on-black: any channel of the BGRA pixel is the coverage.
            let px = std::slice::from_raw_parts(bits as *const u8, size * size * 4);
            for i in 0..size * size {
                alpha[i] = px[i * 4].max(px[i * 4 + 1]).max(px[i * 4 + 2]);
            }
            SelectObject(dc, old_font);
            SelectObject(dc, old_bitmap);
            DeleteObject(font as _);
        }
        if !bitmap.is_null() {
            DeleteObject(bitmap as _);
        }
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);
    }
    alpha
}

/// Battery glyph drawn straight into the RGBA buffer: outline, tip, and a
/// fill proportional to the charge.
fn bar_icon(percent: Option<u8>, color: Rgb) -> Icon {
    let size = ICON_SIZE as usize;
    let mut rgba = vec![0u8; size * size * 4];
    let mut put = |x: usize, y: usize, c: Rgb| {
        if x < size && y < size {
            let i = (y * size + x) * 4;
            rgba[i] = c.0;
            rgba[i + 1] = c.1;
            rgba[i + 2] = c.2;
            rgba[i + 3] = 0xff;
        }
    };

    // Body 2..28 x 9..23, tip 28..30 x 13..19, 2px outline.
    for x in 2..28 {
        for y in 9..23 {
            let edge = x < 4 || x >= 26 || y < 11 || y >= 21;
            if edge {
                put(x, y, WHITE);
            }
        }
    }
    for x in 28..30 {
        for y in 13..19 {
            put(x, y, WHITE);
        }
    }
    if let Some(p) = percent {
        let inner_w = 20usize; // 5..25
        let fill = (inner_w * p.min(100) as usize).div_ceil(100);
        for x in 5..5 + fill {
            for y in 12..20 {
                put(x, y, color);
            }
        }
    } else {
        // Unknown: small dash in the middle.
        for x in 12..18 {
            for y in 15..17 {
                put(x, y, GRAY);
            }
        }
    }
    Icon::from_rgba(rgba, size as u32, size as u32).expect("icon from rgba")
}
