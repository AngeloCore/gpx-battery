use crate::hidpp::{self, BatteryStatus, Mouse};
use crate::winutil::MainWaker;
use hidapi::HidApi;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};
use tray_icon::menu::MenuId;

#[derive(Clone)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    /// The freshest reading we have. When `online` is false this is the last
    /// value read before the mouse went unreachable (asleep or switched off —
    /// a receiver cannot tell those apart), or None if it was never read.
    pub battery: Option<BatteryStatus>,
    /// Whether the battery was successfully read on the latest poll.
    pub online: bool,
}

pub enum PollCommand {
    SetInterval(Duration),
    RefreshNow,
    Rescan,
    Shutdown,
}

/// Everything the main thread reacts to; senders call `MainWaker::wake` after
/// pushing so the blocked message loop drains the channel.
pub enum AppEvent {
    Devices(Vec<DeviceStatus>),
    Menu(MenuId),
    /// Settings were edited in the UI; `save` is false for resets that must
    /// not recreate the settings file.
    SettingsChanged {
        save: bool,
    },
}

/// Full device re-enumeration at most this often unless something is offline
/// or a device-change notification arrives.
const FULL_RESCAN_EVERY: Duration = Duration::from_secs(300);

pub fn spawn(
    cmd_rx: Receiver<PollCommand>,
    tx: Sender<AppEvent>,
    waker: MainWaker,
    initial_interval: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("hid-poll".into())
        .spawn(move || run(cmd_rx, tx, waker, initial_interval))
        .expect("failed to spawn poll thread")
}

fn run(
    cmd_rx: Receiver<PollCommand>,
    tx: Sender<AppEvent>,
    waker: MainWaker,
    mut interval: Duration,
) {
    struct RosterEntry {
        id: String,
        name: String,
        last_battery: Option<BatteryStatus>,
    }

    let Ok(mut api) = HidApi::new() else { return };
    let mut mice: Vec<Mouse> = Vec::new();
    // Every mouse seen this session; entries whose hardware is unreachable are
    // still reported (with their last known battery) so the UI can show them
    // as asleep/off, and we keep rescanning until they come back.
    let mut roster: Vec<RosterEntry> = Vec::new();
    let mut need_rescan = true;
    let mut settle_before_rescan = false;
    let mut last_scan = Instant::now() - FULL_RESCAN_EVERY;

    loop {
        if need_rescan || last_scan.elapsed() > FULL_RESCAN_EVERY {
            if settle_before_rescan {
                // Device-change notifications arrive in bursts while Windows
                // re-plumbs the HID stack; settle briefly and collapse them.
                std::thread::sleep(Duration::from_millis(300));
                loop {
                    match cmd_rx.try_recv() {
                        Ok(PollCommand::SetInterval(d)) => interval = d,
                        Ok(PollCommand::Shutdown) | Err(TryRecvError::Disconnected) => return,
                        Ok(_) => {}
                        Err(TryRecvError::Empty) => break,
                    }
                }
            }
            let _ = api.refresh_devices();
            mice = hidpp::discover(&api);
            for mouse in &mice {
                if !roster.iter().any(|e| e.id == mouse.id) {
                    roster.push(RosterEntry {
                        id: mouse.id.clone(),
                        name: mouse.name.clone(),
                        last_battery: None,
                    });
                }
            }
            need_rescan = false;
            settle_before_rescan = false;
            last_scan = Instant::now();
        }

        let statuses: Vec<DeviceStatus> = roster
            .iter_mut()
            .map(|entry| {
                let fresh = mice
                    .iter()
                    .find(|m| m.id == entry.id)
                    .and_then(|m| m.read_battery().ok());
                if let Some(b) = fresh {
                    entry.last_battery = Some(b);
                }
                DeviceStatus {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    battery: entry.last_battery,
                    online: fresh.is_some(),
                }
            })
            .collect();
        // While anything is unreachable, re-enumerate on every tick so a mouse
        // that wakes up or gets switched back on reappears within one poll.
        if statuses.iter().any(|s| !s.online) {
            need_rescan = true;
        }
        if tx.send(AppEvent::Devices(statuses)).is_err() {
            return;
        }
        waker.wake();

        match cmd_rx.recv_timeout(interval) {
            Ok(PollCommand::SetInterval(d)) => interval = d,
            Ok(PollCommand::RefreshNow) => {}
            Ok(PollCommand::Rescan) => {
                need_rescan = true;
                settle_before_rescan = true;
            }
            Ok(PollCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}
