#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod devices;
mod hidpp;
mod logo;
mod startup;
mod tray;
mod ui;
mod winutil;

use config::Settings;
use devices::{AppEvent, DeviceStatus, PollCommand};
use hidpp::ChargeState;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tray::{TrayManager, UserAction};
use tray_icon::menu::MenuEvent;
use winutil::{MainWaker, WM_APP_DEVCHANGE, WM_APP_SHOWUI, WM_APP_WAKE, wide};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DBT_DEVNODES_CHANGED, DefWindowProcW, DispatchMessageW, GetMessageW,
    HWND_BROADCAST, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW, PostQuitMessage,
    PostThreadMessageW, RegisterClassW, RegisterWindowMessageW, TranslateMessage, WM_DEVICECHANGE,
    WM_USER, WNDCLASSW,
};

static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// System-wide message id a second instance broadcasts to say "show yourself".
static SHOWUI_MESSAGE: AtomicU32 = AtomicU32::new(0);

fn showui_message() -> u32 {
    unsafe { RegisterWindowMessageW(wide("gpx-battery-show-ui").as_ptr()) }
}

fn main() {
    ensure_single_instance();
    enable_dark_menus();

    // Force creation of this thread's message queue before anyone can post to it.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        );
    }
    let waker = MainWaker::for_current_thread();
    MAIN_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    SHOWUI_MESSAGE.store(showui_message(), Ordering::Relaxed);
    create_devchange_window();

    let settings = Arc::new(Mutex::new(Settings::load()));
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<PollCommand>();

    let devices_shared: Arc<Mutex<Vec<DeviceStatus>>> = Arc::new(Mutex::new(Vec::new()));

    let initial_interval = Duration::from_secs(settings.lock().unwrap().poll_interval_secs.max(1));
    let _poll_thread = devices::spawn(cmd_rx, tx.clone(), waker, initial_interval);

    ui::init(settings.clone(), devices_shared.clone(), tx.clone(), waker);

    {
        let tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(AppEvent::Menu(event.id().clone()));
            waker.wake();
        }));
    }

    let mut tray = TrayManager::new().expect("failed to create tray icon");
    let _ = tray.update(&[], &settings.lock().unwrap());

    let mut last_devices: Vec<DeviceStatus> = Vec::new();
    // Devices already toasted for this discharge; cleared once they recover.
    let mut notified: HashSet<String> = HashSet::new();
    let mut last_devchange = Instant::now() - Duration::from_secs(10);

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_APP_WAKE => {}
                WM_APP_DEVCHANGE => {
                    if last_devchange.elapsed() > Duration::from_secs(1) {
                        last_devchange = Instant::now();
                        let _ = cmd_tx.send(PollCommand::Rescan);
                    }
                }
                WM_APP_SHOWUI => {
                    ui::show_settings();
                }
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            while let Ok(event) = rx.try_recv() {
                match event {
                    AppEvent::Devices(list) => {
                        let snapshot = absorb_devices(&list, &settings, &mut notified);
                        let _ = tray.update(&list, &snapshot);
                        *devices_shared.lock().unwrap() = list.clone();
                        ui::poke();
                        last_devices = list;
                    }
                    AppEvent::Menu(id) => match tray.handle_menu_event(&id) {
                        Some(UserAction::ToggleDevice(device_id, selected)) => {
                            let snapshot = {
                                let mut s = settings.lock().unwrap();
                                s.set_selected(&device_id, selected);
                                let _ = s.save();
                                s.clone()
                            };
                            let _ = tray.update(&last_devices, &snapshot);
                        }
                        Some(UserAction::RefreshNow) => {
                            let _ = cmd_tx.send(PollCommand::Rescan);
                            let _ = cmd_tx.send(PollCommand::RefreshNow);
                        }
                        Some(UserAction::OpenSettings) => {
                            ui::show_settings();
                        }
                        Some(UserAction::Exit) => {
                            let _ = cmd_tx.send(PollCommand::Shutdown);
                            PostQuitMessage(0);
                        }
                        None => {}
                    },
                    AppEvent::SettingsChanged { save } => {
                        let snapshot = settings.lock().unwrap().clone();
                        if save {
                            let _ = snapshot.save();
                        }
                        let _ = cmd_tx.send(PollCommand::SetInterval(Duration::from_secs(
                            snapshot.poll_interval_secs.max(1),
                        )));
                        let _ = tray.update(&last_devices, &snapshot);
                    }
                }
            }
        }
    }
}

/// Fold a fresh device list into settings (auto-select newly seen mice) and
/// fire low-battery toasts. Returns the settings snapshot to render with.
fn absorb_devices(
    list: &[DeviceStatus],
    settings: &Arc<Mutex<Settings>>,
    notified: &mut HashSet<String>,
) -> Settings {
    let mut s = settings.lock().unwrap();
    // Auto-select newly seen mice in memory only; nothing is written to disk
    // until the user explicitly applies settings or toggles a device.
    for device in list {
        if !s.known_devices.contains(&device.id) {
            s.known_devices.push(device.id.clone());
            s.selected_devices.push(device.id.clone());
        }
    }
    let snapshot = s.clone();
    drop(s);

    if snapshot.notifications_enabled {
        for device in list {
            if !device.online {
                continue;
            }
            let Some(battery) = &device.battery else {
                continue;
            };
            let low = battery.state == ChargeState::Discharging
                && battery.percent <= snapshot.low_battery_threshold;
            if low {
                if notified.insert(device.id.clone()) {
                    toast(&device.name, battery.percent);
                }
            } else if battery.state != ChargeState::Discharging
                || battery.percent > snapshot.low_battery_threshold.saturating_add(10)
            {
                notified.remove(&device.id);
            }
        }
    }
    snapshot
}

fn toast(name: &str, percent: u8) {
    use tauri_winrt_notification::Toast;
    let _ = Toast::new(Toast::POWERSHELL_APP_ID)
        .title("GPX Battery — low battery")
        .text1(&format!("{name} is at {percent}%"))
        .show();
}

fn ensure_single_instance() {
    unsafe {
        let name = wide("Local\\gpx-battery-single-instance");
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            // Another instance is running: ask it to show its settings window
            // so launching the exe again gives visible feedback, then bow out.
            PostMessageW(HWND_BROADCAST, showui_message(), 0, 0);
            std::process::exit(0);
        }
        // Handle is intentionally leaked; the OS holds the mutex for our lifetime.
    }
}

/// Undocumented-but-stable uxtheme exports (Windows 10 1809+) that Explorer
/// itself uses; they make Win32 popup menus render dark.
fn enable_dark_menus() {
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let set_preferred_app_mode = GetProcAddress(lib, 135 as *const u8);
        let flush_menu_themes = GetProcAddress(lib, 136 as *const u8);
        if let Some(f) = set_preferred_app_mode {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(2); // ForceDark
        }
        if let Some(f) = flush_menu_themes {
            let f: extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

/// Hidden top-level window whose only job is receiving WM_DEVICECHANGE
/// broadcasts (message-only windows don't get them) and nudging the poll
/// thread to rescan.
fn create_devchange_window() {
    unsafe {
        let class_name = wide("gpx-battery-devchange");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(devchange_wndproc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = class_name.as_ptr();
        RegisterClassW(&wc);
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            wc.hInstance,
            std::ptr::null(),
        );
    }
}

extern "system" fn devchange_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DEVICECHANGE && wparam as u32 == DBT_DEVNODES_CHANGED {
        let tid = MAIN_THREAD_ID.load(Ordering::Relaxed);
        if tid != 0 {
            unsafe {
                PostThreadMessageW(tid, WM_APP_DEVCHANGE, 0, 0);
            }
        }
    }
    let showui = SHOWUI_MESSAGE.load(Ordering::Relaxed);
    if showui != 0 && msg == showui {
        let tid = MAIN_THREAD_ID.load(Ordering::Relaxed);
        if tid != 0 {
            unsafe {
                PostThreadMessageW(tid, WM_APP_SHOWUI, 0, 0);
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
