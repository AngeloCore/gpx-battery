use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum IconStyle {
    Percentage,
    BatteryBar,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// How often the battery is polled.
    pub poll_interval_secs: u64,
    /// Device ids that get their own tray icon.
    pub selected_devices: Vec<String>,
    /// Every device id ever seen; new ones are auto-selected once.
    pub known_devices: Vec<String>,
    pub icon_style: IconStyle,
    pub low_battery_threshold: u8,
    pub notifications_enabled: bool,
    /// Show the welcome window when the app starts.
    pub show_welcome: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_interval_secs: 60,
            selected_devices: Vec::new(),
            known_devices: Vec::new(),
            icon_style: IconStyle::Percentage,
            low_battery_threshold: 15,
            notifications_enabled: false,
            show_welcome: true,
        }
    }
}

/// The only place the app ever stores anything: %APPDATA%\gpx-battery.
fn appdata_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("gpx-battery"))
}

fn settings_path() -> Option<PathBuf> {
    Some(appdata_dir()?.join("settings.json"))
}

/// The settings file currently on disk, if one exists.
pub fn existing_settings_path() -> Option<PathBuf> {
    settings_path().filter(|p| p.exists())
}

/// Remove every file the app has created.
pub fn delete_app_files() {
    if let Some(dir) = appdata_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

impl Settings {
    /// Defaults unless the user has explicitly saved settings before.
    pub fn load() -> Self {
        if let Some(path) = settings_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(settings) = serde_json::from_str(&text) {
                    return settings;
                }
            }
        }
        Self::default()
    }

    /// Only called on explicit user action (Apply / device toggle); the file
    /// is created on first save.
    pub fn save(&self) -> Result<()> {
        let Some(path) = settings_path() else {
            anyhow::bail!("no writable settings location");
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected_devices.iter().any(|d| d == id)
    }

    pub fn set_selected(&mut self, id: &str, on: bool) {
        if on {
            if !self.is_selected(id) {
                self.selected_devices.push(id.to_string());
            }
        } else {
            self.selected_devices.retain(|d| d != id);
        }
    }
}
