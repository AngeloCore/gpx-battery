//! Minimal HID++ 2.0 client: just enough to find Logitech wireless mice and
//! read their battery level, talking straight to the Windows HID stack.

use anyhow::{Result, bail};
use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

pub const LOGITECH_VID: u16 = 0x046d;

const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
/// Arbitrary software id (1..=15) echoed back in responses so we can match them.
const SW_ID: u8 = 0x0a;

const FEAT_ROOT: u8 = 0x00;
const FEAT_DEVICE_INFO: u16 = 0x0003;
const FEAT_NAME_TYPE: u16 = 0x0005;
const FEAT_BATTERY_STATUS: u16 = 0x1000;
const FEAT_BATTERY_VOLTAGE: u16 = 0x1001;
const FEAT_UNIFIED_BATTERY: u16 = 0x1004;

const DEVICE_TYPE_MOUSE: u8 = 3;

const TIMEOUT_MS: u64 = 2000;
/// Shorter timeout while probing receiver slots that may be empty.
const PROBE_TIMEOUT_MS: u64 = 400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChargeState {
    Discharging,
    Charging,
    Full,
}

#[derive(Clone, Copy, Debug)]
pub struct BatteryStatus {
    pub percent: u8,
    pub state: ChargeState,
}

#[derive(Clone, Copy)]
enum BatteryFeature {
    /// 0x1004 Unified Battery (G Pro X Superlight 2 and other recent devices)
    Unified(u8),
    /// 0x1000 Battery Unified Level Status
    Status(u8),
    /// 0x1001 Battery Voltage (older G mice); voltage is mapped to a percentage
    Voltage(u8),
}

pub struct Mouse {
    dev: HidDevice,
    device_index: u8,
    battery: BatteryFeature,
    pub id: String,
    pub name: String,
}

impl Mouse {
    pub fn read_battery(&self) -> Result<BatteryStatus> {
        match self.battery {
            BatteryFeature::Unified(fi) => {
                // getStatus -> [state_of_charge %, level flags, charging_status, external power]
                let r = request(&self.dev, self.device_index, fi, 0x1, &[], TIMEOUT_MS)?;
                let state = match r[2] {
                    1 | 2 => ChargeState::Charging,
                    3 => ChargeState::Full,
                    _ => ChargeState::Discharging,
                };
                Ok(BatteryStatus {
                    percent: r[0].min(100),
                    state,
                })
            }
            BatteryFeature::Status(fi) => {
                // getBatteryLevelStatus -> [level %, next level, charging status]
                let r = request(&self.dev, self.device_index, fi, 0x0, &[], TIMEOUT_MS)?;
                let state = match r[2] {
                    1 | 2 | 4 => ChargeState::Charging,
                    3 => ChargeState::Full,
                    _ => ChargeState::Discharging,
                };
                Ok(BatteryStatus {
                    percent: r[0].min(100),
                    state,
                })
            }
            BatteryFeature::Voltage(fi) => {
                // getBatteryInfo -> [voltage mV (BE u16), flags]
                let r = request(&self.dev, self.device_index, fi, 0x0, &[], TIMEOUT_MS)?;
                let mv = u16::from_be_bytes([r[0], r[1]]);
                let percent = voltage_to_percent(mv);
                let state = if r[2] & 0x80 != 0 {
                    if percent >= 100 {
                        ChargeState::Full
                    } else {
                        ChargeState::Charging
                    }
                } else {
                    ChargeState::Discharging
                };
                Ok(BatteryStatus { percent, state })
            }
        }
    }
}

/// Find every HID++ 2.0 wireless Logitech mouse currently reachable: directly
/// attached ones (Bluetooth / USB cable) and ones paired to a Unifying or
/// Lightspeed receiver.
pub fn discover(api: &HidApi) -> Vec<Mouse> {
    let mut found: Vec<Mouse> = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for info in api.device_list() {
        if info.vendor_id() != LOGITECH_VID {
            continue;
        }
        // HID++ long-report collections: 0xFF00/2 on Unifying receivers,
        // 0xFF43 on Lightspeed receivers and directly attached modern devices.
        let hidpp =
            (info.usage_page() == 0xff00 && info.usage() == 0x0002) || info.usage_page() == 0xff43;
        if !hidpp || !seen_paths.insert(info.path().to_owned()) {
            continue;
        }

        // Receivers occupy the 0xC5xx product-id range and multiplex up to six
        // paired devices; anything else talks HID++ directly at index 0xFF.
        let indices: &[u8] = if (0xc500..=0xc5ff).contains(&info.product_id()) {
            &[1, 2, 3, 4, 5, 6]
        } else {
            &[0xff]
        };

        for &index in indices {
            let Ok(dev) = api.open_path(info.path()) else {
                break;
            };
            if let Ok(Some(mouse)) = probe(dev, index) {
                if !found.iter().any(|m| m.id == mouse.id) {
                    found.push(mouse);
                }
            }
        }
    }
    found
}

/// Check whether a HID++ 2.0 mouse with a readable battery answers at
/// `device_index` on this interface, and set it up if so.
fn probe(dev: HidDevice, device_index: u8) -> Result<Option<Mouse>> {
    // Root ping doubles as protocol-version query; HID++ 1.0 devices and empty
    // receiver slots answer with an error, dead interfaces time out.
    let Ok(ver) = request(
        &dev,
        device_index,
        FEAT_ROOT,
        0x1,
        &[0, 0, 0x55],
        PROBE_TIMEOUT_MS,
    ) else {
        return Ok(None);
    };
    if ver[0] < 2 {
        return Ok(None);
    }

    let Some(name_feat) = get_feature_index(&dev, device_index, FEAT_NAME_TYPE)? else {
        return Ok(None);
    };
    let device_type = request(&dev, device_index, name_feat, 0x2, &[], TIMEOUT_MS)?[0];
    if device_type != DEVICE_TYPE_MOUSE {
        return Ok(None);
    }

    let battery = if let Some(fi) = get_feature_index(&dev, device_index, FEAT_UNIFIED_BATTERY)? {
        BatteryFeature::Unified(fi)
    } else if let Some(fi) = get_feature_index(&dev, device_index, FEAT_BATTERY_STATUS)? {
        BatteryFeature::Status(fi)
    } else if let Some(fi) = get_feature_index(&dev, device_index, FEAT_BATTERY_VOLTAGE)? {
        BatteryFeature::Voltage(fi)
    } else {
        return Ok(None);
    };

    let name =
        read_name(&dev, device_index, name_feat).unwrap_or_else(|_| "Logitech mouse".to_string());
    // Prefer the unit id burned into the device so the same mouse keeps its
    // identity whether it shows up via receiver, Bluetooth or USB cable.
    let id = read_unit_id(&dev, device_index).unwrap_or_else(|_| {
        let path = dev
            .get_device_info()
            .map(|i| i.path().to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{path}#{device_index}")
    });

    Ok(Some(Mouse {
        dev,
        device_index,
        battery,
        id,
        name,
    }))
}

fn get_feature_index(dev: &HidDevice, device_index: u8, feature: u16) -> Result<Option<u8>> {
    let r = request(
        dev,
        device_index,
        FEAT_ROOT,
        0x0,
        &[(feature >> 8) as u8, feature as u8],
        TIMEOUT_MS,
    )?;
    Ok(if r[0] == 0 { None } else { Some(r[0]) })
}

fn read_name(dev: &HidDevice, device_index: u8, feat: u8) -> Result<String> {
    let total = request(dev, device_index, feat, 0x0, &[], TIMEOUT_MS)?[0] as usize;
    let mut bytes = Vec::with_capacity(total);
    while bytes.len() < total {
        let r = request(
            dev,
            device_index,
            feat,
            0x1,
            &[bytes.len() as u8],
            TIMEOUT_MS,
        )?;
        let take = (total - bytes.len()).min(16);
        if take == 0 {
            break;
        }
        bytes.extend_from_slice(&r[..take]);
    }
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string())
}

fn read_unit_id(dev: &HidDevice, device_index: u8) -> Result<String> {
    let Some(fi) = get_feature_index(dev, device_index, FEAT_DEVICE_INFO)? else {
        bail!("device info feature not present");
    };
    // getDeviceInfo -> [entity count, unit id (4), transport (2), model id (6), ...]
    let r = request(dev, device_index, fi, 0x0, &[], TIMEOUT_MS)?;
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    Ok(format!("{}-{}", hex(&r[1..5]), hex(&r[7..13])))
}

/// Send one HID++ 2.0 long request and wait for the matching response,
/// skipping unrelated notifications that share the interface.
fn request(
    dev: &HidDevice,
    device_index: u8,
    feature_index: u8,
    function: u8,
    params: &[u8],
    timeout_ms: u64,
) -> Result<[u8; 16]> {
    debug_assert!(params.len() <= 16);
    let fn_sw = (function << 4) | SW_ID;
    let mut report = [0u8; LONG_LEN];
    report[0] = REPORT_LONG;
    report[1] = device_index;
    report[2] = feature_index;
    report[3] = fn_sw;
    report[4..4 + params.len()].copy_from_slice(params);
    dev.write(&report)?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut buf = [0u8; 64];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("HID++ response timeout");
        }
        let n = dev.read_timeout(&mut buf, remaining.as_millis() as i32)?;
        if n == 0 {
            bail!("HID++ response timeout");
        }
        let r = &buf[..n];
        if r.len() < 5 || r[1] != device_index {
            continue;
        }
        // HID++ 2.0 error: [0x11, index, 0xFF, feature, fn|sw, error]
        if r[0] == REPORT_LONG && r[2] == 0xff && r[3] == feature_index && r[4] == fn_sw {
            bail!("HID++ 2.0 error {}", r[5]);
        }
        // HID++ 1.0 error from a receiver: [0x10, index, 0x8F, feature, fn|sw, error]
        if r[0] == REPORT_SHORT && r[2] == 0x8f && r[3] == feature_index && r[4] == fn_sw {
            bail!("HID++ 1.0 error {}", *r.get(5).unwrap_or(&0));
        }
        if r[0] == REPORT_LONG && r[2] == feature_index && r[3] == fn_sw {
            let mut out = [0u8; 16];
            let len = (r.len() - 4).min(16);
            out[..len].copy_from_slice(&r[4..4 + len]);
            return Ok(out);
        }
        // Anything else is an unsolicited event; keep waiting.
    }
}

/// Li-ion discharge curve thresholds used by Solaar and the Linux kernel
/// driver for feature 0x1001 devices.
fn voltage_to_percent(mv: u16) -> u8 {
    const CURVE: [(u16, u8); 13] = [
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];
    for (v, p) in CURVE {
        if mv >= v {
            return p;
        }
    }
    0
}
