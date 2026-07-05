//! The app's UI window (welcome screen + settings screen).
//!
//! winit allows only one event loop per process, so a single UI thread is
//! started at launch and lives for the app's lifetime. The window itself is
//! created on demand and fully destroyed when closed (`run_and_return`):
//! while no window is open, no GL context, fonts or textures exist — the
//! idle thread costs a few KB instead of tens of MB.

use crate::config::{self, IconStyle, Settings};
use crate::devices::{AppEvent, DeviceStatus};
use crate::logo;
use crate::startup;
use crate::winutil::MainWaker;
use eframe::egui::{self, Color32, CornerRadius, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Context of the currently open window, if any.
static UI_CTX: Mutex<Option<egui::Context>> = Mutex::new(None);
/// Asks an already-open window to switch to the settings screen.
static SHOW_SETTINGS: AtomicBool = AtomicBool::new(false);
/// Asks the UI thread to open a new window on the given screen.
static UI_TX: OnceLock<Sender<Screen>> = OnceLock::new();

const BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
const CARD: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x21);
const WIDGET: Color32 = Color32::from_rgb(0x27, 0x27, 0x2c);
const WIDGET_HOVER: Color32 = Color32::from_rgb(0x32, 0x32, 0x38);
const ACCENT: Color32 = Color32::from_rgb(0x00, 0xb8, 0xfc);
const TEXT: Color32 = Color32::from_rgb(0xec, 0xec, 0xee);
const TEXT_WEAK: Color32 = Color32::from_rgb(0x93, 0x93, 0x9c);
const DANGER: Color32 = Color32::from_rgb(0xd6, 0x3a, 0x3a);

const WELCOME_SIZE: [f32; 2] = [400.0, 330.0];
const SETTINGS_SIZE: [f32; 2] = [440.0, 660.0];

/// Spawn the UI thread. It opens the welcome window at startup (unless the
/// user disabled it), then sleeps on its channel until a window is requested;
/// each window is created fresh and torn down completely when closed.
pub fn init(
    settings: Arc<Mutex<Settings>>,
    devices: Arc<Mutex<Vec<DeviceStatus>>>,
    tx: Sender<AppEvent>,
    waker: MainWaker,
) {
    let (ui_tx, ui_rx) = mpsc::channel::<Screen>();
    let _ = UI_TX.set(ui_tx);
    let show_welcome = settings.lock().unwrap().show_welcome;
    std::thread::Builder::new()
        .name("ui".into())
        .spawn(move || {
            if show_welcome {
                run_window(Screen::Welcome, &settings, &devices, &tx, waker);
            }
            while let Ok(mut screen) = ui_rx.recv() {
                // Collapse requests queued while a window was open.
                while let Ok(next) = ui_rx.try_recv() {
                    screen = next;
                }
                loop {
                    run_window(screen, &settings, &devices, &tx, waker);
                    // A request that raced with the window closing would be
                    // lost as a no-op repaint; honor it by reopening.
                    if SHOW_SETTINGS.swap(false, Ordering::SeqCst) {
                        screen = Screen::Settings;
                    } else {
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn ui thread");
}

/// Create a window and block until the user closes it. `run_and_return`
/// makes eframe reuse this thread's event loop and destroy the window, GL
/// context and all UI memory when `run_native` returns.
fn run_window(
    screen: Screen,
    settings: &Arc<Mutex<Settings>>,
    devices: &Arc<Mutex<Vec<DeviceStatus>>>,
    tx: &Sender<AppEvent>,
    waker: MainWaker,
) {
    let size = match screen {
        Screen::Welcome => WELCOME_SIZE,
        Screen::Settings => SETTINGS_SIZE,
    };
    let icon = egui::IconData {
        rgba: logo::logo_rgba(64),
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GPX Battery")
            .with_inner_size(size)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(Arc::new(icon)),
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        run_and_return: true,
        ..Default::default()
    };
    let (settings, devices, tx) = (settings.clone(), devices.clone(), tx.clone());
    let _ = eframe::run_native(
        "GPX Battery",
        options,
        Box::new(move |cc| {
            apply_style(&cc.egui_ctx);
            *UI_CTX.lock().unwrap() = Some(cc.egui_ctx.clone());
            Ok(Box::new(UiApp::new(screen, settings, devices, tx, waker)))
        }),
    );
    *UI_CTX.lock().unwrap() = None;

    // The UI is freed at this point, but pages the window (and the GPU
    // driver) touched are still resident. Hand them back to the OS; anything
    // still needed is paged back in on demand.
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Bring up the settings screen (from the tray menu, any thread).
pub fn show_settings() {
    // If a window is already open, switch it in place and focus it …
    if let Some(ctx) = UI_CTX.lock().unwrap().clone() {
        SHOW_SETTINGS.store(true, Ordering::SeqCst);
        ctx.request_repaint();
        return;
    }
    // … otherwise ask the UI thread for a fresh one.
    if let Some(ui_tx) = UI_TX.get() {
        let _ = ui_tx.send(Screen::Settings);
    }
}

/// Let the window (if one is open) refresh after a device update.
pub fn poke() {
    if let Some(ctx) = UI_CTX.lock().unwrap().clone() {
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
        screen: Screen,
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
            screen,
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
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::from(
            SETTINGS_SIZE,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Actually closes the window; the whole UI stack is torn down and
    /// rebuilt on the next open, so no memory is held while it's gone.
    fn hide(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                    // Fixed-height, two-line storage block so switching between
                    // "no file" and "file exists" never shifts the layout below.
                    let width = ui.available_width();
                    ui.allocate_ui(egui::vec2(width, 38.0), |ui| {
                        ui.set_min_size(egui::vec2(width, 38.0));
                        ui.spacing_mut().item_spacing.y = 3.0;
                        match config::existing_settings_path() {
                            Some(path) => {
                                ui.label(RichText::new("Settings file:").color(TEXT_WEAK));
                                ui.label(
                                    RichText::new(path.display().to_string())
                                        .color(ACCENT)
                                        .monospace()
                                        .size(12.0),
                                );
                            }
                            None => {
                                ui.label(RichText::new("Using default settings.").color(TEXT_WEAK));
                                ui.label(
                                    RichText::new(
                                        "A settings file will be created in \
                                         %APPDATA%\\gpx-battery when you click Apply.",
                                    )
                                    .color(TEXT_WEAK)
                                    .size(12.0),
                                );
                            }
                        }
                    });

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
                    toggle(ui, &mut self.start_on_boot, "Start with Windows");
                    toggle(ui, &mut self.show_welcome, "Show welcome window at startup");

                    section(ui, "TRAY ICON");
                    segmented(
                        ui,
                        &mut self.icon_style,
                        &[
                            (IconStyle::Percentage, "Percentage number"),
                            (IconStyle::BatteryBar, "Battery bar"),
                        ],
                    );

                    section(ui, "NOTIFICATIONS");
                    toggle(ui, &mut self.notifications, "Notify on low battery");
                    ui.add_enabled_ui(self.notifications, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Threshold");
                            percent_slider(ui, &mut self.threshold, 5, 99);
                            ui.label(RichText::new(format!("{}%", self.threshold)).strong());
                        });
                    });
                });
        });
    }
}

/// G HUB-style toggle switch: a pill that slides its knob and fades from
/// gray to accent blue. Used instead of stock checkboxes.
fn toggle(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    const OFF_BG: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x42);
    ui.horizontal(|ui| {
        let (rect, mut response) =
            ui.allocate_exact_size(egui::vec2(40.0, 22.0), egui::Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let t = ui.ctx().animate_bool_responsive(response.id, *on);
            let bg = egui::Rgba::from(OFF_BG) * (1.0 - t) + egui::Rgba::from(ACCENT) * t;
            let radius = rect.height() / 2.0;
            ui.painter().rect_filled(rect, radius, Color32::from(bg));
            let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
            ui.painter().circle_filled(
                egui::pos2(knob_x, rect.center().y),
                radius - 3.0,
                Color32::WHITE,
            );
        }
        let label = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
        if label.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        response
    })
    .inner
}

/// G HUB-style segmented control: a gray track holding one pill per option,
/// with the active one filled accent blue. Modern replacement for a radio
/// group where the options are mutually exclusive.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, options: &[(T, &str)]) {
    const GAP: f32 = 4.0;
    egui::Frame::new()
        .fill(WIDGET)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.horizontal(|ui| {
                let n = options.len() as f32;
                let seg_w = ((ui.available_width() - GAP * (n - 1.0)) / n).max(0.0);
                for (i, &(value, label)) in options.iter().enumerate() {
                    let selected = *current == value;
                    // Crossfade fill and text so the selection glides between
                    // segments instead of snapping.
                    let t = ui
                        .ctx()
                        .animate_bool_responsive(ui.id().with(("segment", i)), selected);
                    let fill = Color32::from(
                        egui::Rgba::from(Color32::TRANSPARENT) * (1.0 - t)
                            + egui::Rgba::from(ACCENT) * t,
                    );
                    let text_color = Color32::from(
                        egui::Rgba::from(TEXT) * (1.0 - t) + egui::Rgba::from(Color32::BLACK) * t,
                    );
                    let button = egui::Button::new(RichText::new(label).color(text_color).strong())
                        .fill(fill)
                        .corner_radius(CornerRadius::same(6))
                        .min_size(egui::vec2(seg_w, 28.0));
                    if ui.add(button).clicked() {
                        *current = value;
                    }
                }
            });
        });
}

/// Custom slider: slim rounded track with accent fill and a white knob that
/// grows slightly on hover/drag.
fn percent_slider(ui: &mut egui::Ui, value: &mut u8, min: u8, max: u8) -> egui::Response {
    const TRACK_BG: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x42);
    let knob_r = 8.0;
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(230.0, 24.0), egui::Sense::click_and_drag());
    let track_left = rect.left() + knob_r;
    let track_right = rect.right() - knob_r;

    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.x - track_left) / (track_right - track_left)).clamp(0.0, 1.0);
            let new = (min as f32 + t * (max - min) as f32).round() as u8;
            if new != *value {
                *value = new;
                response.mark_changed();
            }
        }
    }

    if ui.is_rect_visible(rect) {
        let t = ((*value).clamp(min, max) as f32 - min as f32) / (max - min) as f32;
        let cy = rect.center().y;
        let track = egui::Rect::from_min_max(
            egui::pos2(track_left, cy - 3.0),
            egui::pos2(track_right, cy + 3.0),
        );
        ui.painter().rect_filled(track, 3.0, TRACK_BG);
        let knob_x = egui::lerp(track_left..=track_right, t.clamp(0.0, 1.0));
        let fill = egui::Rect::from_min_max(
            egui::pos2(track_left, cy - 3.0),
            egui::pos2(knob_x, cy + 3.0),
        );
        ui.painter().rect_filled(fill, 3.0, ACCENT);
        let grow = ui
            .ctx()
            .animate_bool_responsive(response.id, response.hovered() || response.dragged());
        ui.painter().circle_filled(
            egui::pos2(knob_x, cy),
            knob_r - 1.5 + grow * 2.0,
            Color32::WHITE,
        );
    }
    response
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
        match self.screen {
            Screen::Welcome => self.welcome_ui(ui),
            Screen::Settings => self.settings_ui(ui),
        }
    }
}
