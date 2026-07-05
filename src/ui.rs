//! The app's single UI window (welcome screen + settings screen).
//!
//! winit allows only one event loop per process, so this thread is started
//! once at launch and lives for the app's lifetime; "closing" the window just
//! hides it and reopening only makes it visible again.

use crate::config::{self, IconStyle, Settings};
use crate::devices::{AppEvent, DeviceStatus};
use crate::logo;
use crate::startup;
use crate::winutil::MainWaker;
use eframe::egui::{self, Color32, CornerRadius, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

static UI_CTX: OnceLock<egui::Context> = OnceLock::new();
static SHOW_SETTINGS: AtomicBool = AtomicBool::new(false);

const BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
const CARD: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x21);
const WIDGET: Color32 = Color32::from_rgb(0x27, 0x27, 0x2c);
const WIDGET_HOVER: Color32 = Color32::from_rgb(0x32, 0x32, 0x38);
const ACCENT: Color32 = Color32::from_rgb(0x00, 0xb8, 0xfc);
const TEXT: Color32 = Color32::from_rgb(0xec, 0xec, 0xee);
const TEXT_WEAK: Color32 = Color32::from_rgb(0x93, 0x93, 0x9c);
const DANGER: Color32 = Color32::from_rgb(0xd6, 0x3a, 0x3a);

/// Spawn the UI thread. The window starts visible on the welcome screen
/// unless the user disabled it.
pub fn init(
    settings: Arc<Mutex<Settings>>,
    devices: Arc<Mutex<Vec<DeviceStatus>>>,
    tx: Sender<AppEvent>,
    waker: MainWaker,
) {
    let visible = settings.lock().unwrap().show_welcome;
    std::thread::Builder::new()
        .name("ui".into())
        .spawn(move || {
            let icon = egui::IconData {
                rgba: logo::logo_rgba(64),
                width: 64,
                height: 64,
            };
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_title("GPX Battery")
                    .with_inner_size([400.0, 330.0])
                    .with_resizable(false)
                    .with_maximize_button(false)
                    .with_visible(visible)
                    .with_icon(Arc::new(icon)),
                event_loop_builder: Some(Box::new(|builder| {
                    use winit::platform::windows::EventLoopBuilderExtWindows;
                    builder.with_any_thread(true);
                })),
                ..Default::default()
            };
            let _ = eframe::run_native(
                "GPX Battery",
                options,
                Box::new(move |cc| {
                    apply_style(&cc.egui_ctx);
                    let _ = UI_CTX.set(cc.egui_ctx.clone());
                    Ok(Box::new(UiApp::new(settings, devices, tx, waker)))
                }),
            );
        })
        .expect("failed to spawn ui thread");
}

/// Bring up the settings screen (from the tray menu, any thread).
pub fn show_settings() {
    SHOW_SETTINGS.store(true, Ordering::SeqCst);
    if let Some(ctx) = UI_CTX.get() {
        ctx.request_repaint();
    }
}

/// Let the (possibly visible) window refresh after a device update.
pub fn poke() {
    if let Some(ctx) = UI_CTX.get() {
        ctx.request_repaint();
    }
}

/// G HUB-like dark theme: near-black panels, Logi-blue accent, rounded widgets.
fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = Color32::from_rgb(0x0e, 0x0e, 0x10);
        v.faint_bg_color = CARD;
        v.override_text_color = Some(TEXT);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.linear_multiply(0.6);
        v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        v.slider_trailing_fill = true;

        v.widgets.noninteractive.bg_fill = CARD;
        v.widgets.inactive.bg_fill = WIDGET;
        v.widgets.inactive.weak_bg_fill = WIDGET;
        v.widgets.hovered.bg_fill = WIDGET_HOVER;
        v.widgets.hovered.weak_bg_fill = WIDGET_HOVER;
        v.widgets.active.bg_fill = ACCENT.linear_multiply(0.35);
        v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.35);
        v.widgets.open.bg_fill = WIDGET_HOVER;
        v.widgets.open.weak_bg_fill = WIDGET_HOVER;
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(6);
            w.fg_stroke.color = TEXT;
            w.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x33, 0x33, 0x3a));
        }
        v.widgets.noninteractive.fg_stroke.color = TEXT_WEAK;

        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        // Rows allocate this height up front, so short labels and taller
        // widgets (drag values, combo boxes) center on the same line.
        style.spacing.interact_size = egui::vec2(40.0, 26.0);
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Welcome,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Seconds,
    Minutes,
    Hours,
}

impl Unit {
    fn label(self) -> &'static str {
        match self {
            Unit::Seconds => "seconds",
            Unit::Minutes => "minutes",
            Unit::Hours => "hours",
        }
    }

    fn multiplier(self) -> u64 {
        match self {
            Unit::Seconds => 1,
            Unit::Minutes => 60,
            Unit::Hours => 3600,
        }
    }
}

struct UiApp {
    settings: Arc<Mutex<Settings>>,
    devices: Arc<Mutex<Vec<DeviceStatus>>>,
    tx: Sender<AppEvent>,
    waker: MainWaker,
    screen: Screen,
    logo_tex: Option<egui::TextureHandle>,
    // Draft (unapplied) settings shown by the settings screen.
    interval_value: u64,
    unit: Unit,
    start_on_boot: bool,
    icon_style: IconStyle,
    notifications: bool,
    threshold: u8,
    show_welcome: bool,
    confirm_delete: bool,
}

impl UiApp {
    fn new(
        settings: Arc<Mutex<Settings>>,
        devices: Arc<Mutex<Vec<DeviceStatus>>>,
        tx: Sender<AppEvent>,
        waker: MainWaker,
    ) -> Self {
        let mut app = Self {
            settings,
            devices,
            tx,
            waker,
            screen: Screen::Welcome,
            logo_tex: None,
            interval_value: 60,
            unit: Unit::Seconds,
            start_on_boot: false,
            icon_style: IconStyle::Percentage,
            notifications: true,
            threshold: 15,
            show_welcome: true,
            confirm_delete: false,
        };
        app.reload();
        app
    }

    /// Refresh drafts from the applied settings.
    fn reload(&mut self) {
        let snapshot = self.settings.lock().unwrap().clone();
        let secs = snapshot.poll_interval_secs.max(1);
        (self.interval_value, self.unit) = if secs % 3600 == 0 {
            (secs / 3600, Unit::Hours)
        } else if secs % 60 == 0 {
            (secs / 60, Unit::Minutes)
        } else {
            (secs, Unit::Seconds)
        };
        self.start_on_boot = startup::is_enabled();
        self.icon_style = snapshot.icon_style;
        self.notifications = snapshot.notifications_enabled;
        self.threshold = snapshot.low_battery_threshold;
        self.show_welcome = snapshot.show_welcome;
        self.confirm_delete = false;
    }

    /// True when any draft differs from the applied settings, so reverting an
    /// edit by hand disables Apply again.
    fn is_dirty(&self) -> bool {
        let s = self.settings.lock().unwrap();
        (self.interval_value * self.unit.multiplier()).max(1) != s.poll_interval_secs
            || self.icon_style != s.icon_style
            || self.notifications != s.notifications_enabled
            || self.threshold != s.low_battery_threshold
            || self.show_welcome != s.show_welcome
            || self.start_on_boot != startup::is_enabled()
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.reload();
        self.screen = Screen::Settings;
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(440.0, 560.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.confirm_delete = false;
    }

    fn apply(&mut self) {
        {
            let mut s = self.settings.lock().unwrap();
            s.poll_interval_secs = (self.interval_value * self.unit.multiplier()).max(1);
            s.icon_style = self.icon_style;
            s.notifications_enabled = self.notifications;
            s.low_battery_threshold = self.threshold;
            s.show_welcome = self.show_welcome;
        }
        let _ = startup::set_enabled(self.start_on_boot);
        self.start_on_boot = startup::is_enabled();
        let _ = self.tx.send(AppEvent::SettingsChanged { save: true });
        self.waker.wake();
    }

    fn delete_app_data(&mut self) {
        config::delete_app_files();
        let _ = startup::set_enabled(false);
        *self.settings.lock().unwrap() = Settings::default();
        let _ = self.tx.send(AppEvent::SettingsChanged { save: false });
        self.waker.wake();
        self.reload();
    }

    fn logo_texture(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.logo_tex
            .get_or_insert_with(|| {
                let px = logo::logo_rgba(96);
                let image = egui::ColorImage::from_rgba_unmultiplied([96, 96], &px);
                ctx.load_texture("logo", image, Default::default())
            })
            .id()
    }

    fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let logo = self.logo_texture(&ctx);
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.image((logo, egui::vec2(72.0, 72.0)));
            ui.add_space(4.0);
            ui.label(RichText::new("GPX Battery").size(24.0).strong());
            ui.add_space(2.0);
            ui.label("The app is running in the system tray.");
            ui.label(
                RichText::new("Look for the battery icon next to the clock.").color(TEXT_WEAK),
            );
            ui.add_space(10.0);
            let first = self.devices.lock().unwrap().first().cloned();
            match first {
                Some(device) => {
                    let status = match (&device.battery, device.online) {
                        (Some(b), true) => format!("{}%", b.percent),
                        (Some(b), false) => format!("{}% (asleep)", b.percent),
                        (None, _) => "off".to_string(),
                    };
                    ui.label(
                        RichText::new(format!("{} — {}", device.name, status))
                            .color(ACCENT)
                            .strong(),
                    );
                }
                None => {
                    ui.label(RichText::new("Searching for Logitech mice…").color(TEXT_WEAK));
                }
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let total = 190.0;
                ui.add_space((ui.available_width() - total).max(0.0) / 2.0);
                if ui.add(accent_button("Open settings")).clicked() {
                    self.open_settings(&ctx);
                }
                if ui.button("Hide").clicked() {
                    self.hide(&ctx);
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        egui::Panel::bottom(egui::Id::new("actions"))
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.is_dirty(), accent_button("Apply"))
                        .clicked()
                    {
                        self.apply();
                    }
                    if ui.button("Hide").clicked() {
                        self.hide(&ctx);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let deletable = config::existing_settings_path().is_some();
                        if !deletable {
                            self.confirm_delete = false;
                        }
                        let label = if self.confirm_delete {
                            "Click again to confirm"
                        } else {
                            "Delete app data"
                        };
                        let button =
                            egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
                                .fill(DANGER)
                                .corner_radius(CornerRadius::same(6));
                        if ui.add_enabled(deletable, button).clicked() {
                            if self.confirm_delete {
                                self.delete_app_data();
                            } else {
                                self.confirm_delete = true;
                            }
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Delete app data removes the app's %APPDATA% folder, disables \
                         autostart and resets all settings to their defaults.",
                    )
                    .color(TEXT_WEAK),
                );
            });

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.label(RichText::new("Settings").size(20.0).strong());
                    match config::existing_settings_path() {
                        Some(path) => {
                            ui.label(
                                RichText::new(format!("Settings file: {}", path.display()))
                                    .color(TEXT_WEAK),
                            );
                        }
                        None => {
                            ui.label(
                                RichText::new(
                                    "Using default settings. The settings file will be \
                                     created in %APPDATA%\\gpx-battery when you click Apply.",
                                )
                                .color(TEXT_WEAK),
                            );
                        }
                    }

                    section(ui, "GENERAL");
                    ui.horizontal(|ui| {
                        ui.label("Check battery every");
                        ui.add(egui::DragValue::new(&mut self.interval_value).range(1..=999));
                        egui::ComboBox::from_id_salt("interval-unit")
                            .selected_text(self.unit.label())
                            .show_ui(ui, |ui| {
                                for unit in [Unit::Seconds, Unit::Minutes, Unit::Hours] {
                                    ui.selectable_value(&mut self.unit, unit, unit.label());
                                }
                            });
                    });
                    ui.label(
                        RichText::new(
                            "Each check takes a few milliseconds; the app sleeps in between.",
                        )
                        .color(TEXT_WEAK),
                    );
                    ui.checkbox(&mut self.start_on_boot, "Start with Windows");
                    ui.checkbox(&mut self.show_welcome, "Show welcome window at startup");

                    section(ui, "TRAY ICON");
                    ui.radio_value(
                        &mut self.icon_style,
                        IconStyle::Percentage,
                        "Percentage number",
                    );
                    ui.radio_value(&mut self.icon_style, IconStyle::BatteryBar, "Battery bar");

                    section(ui, "NOTIFICATIONS");
                    ui.checkbox(&mut self.notifications, "Notify on low battery");
                    ui.add_enabled_ui(self.notifications, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Threshold");
                            ui.add(egui::Slider::new(&mut self.threshold, 5..=50).suffix("%"));
                        });
                    });
                });
        });
    }
}

fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).color(Color32::BLACK).strong())
        .fill(ACCENT)
        .corner_radius(CornerRadius::same(6))
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).small().strong().color(ACCENT));
    ui.separator();
}

impl eframe::App for UiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if SHOW_SETTINGS.swap(false, Ordering::SeqCst) {
            self.open_settings(&ctx);
        }
        // The X button hides the window instead of destroying it, because the
        // event loop must survive for the window to ever come back.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(&ctx);
        }
        match self.screen {
            Screen::Welcome => self.welcome_ui(ui),
            Screen::Settings => self.settings_ui(ui),
        }
    }
}
