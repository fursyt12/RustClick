//! Logitech HID++ 2.0 over hidapi.
//!
//! HID++ is Logitech's vendor protocol on a separate HID collection of the
//! receiver or device. Its REPROG_CONTROLS_V4 feature (0x1B04) can *divert*
//! a button: the remote then stops sending its keystroke and reports the
//! press as a HID++ notification instead. That gives device identification
//! and suppression without hooks or drivers, and is what Logitech's own
//! presentation software uses. Which controls a given remote exposes is
//! recorded with `rustclick-diag` before the input provider relies on it.

use std::ffi::{CStr, CString};
use std::fmt;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

pub const LOGITECH_VID: u16 = 0x046D;
const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
/// Software id in requests; notifications carry 0.
const SW_ID: u8 = 0x0A;
/// Device index of a directly connected (USB cable or Bluetooth) device.
pub const DIRECT: u8 = 0xFF;

pub const FEATURE_ROOT: u16 = 0x0000;
pub const FEATURE_SET: u16 = 0x0001;
pub const FEATURE_DEVICE_NAME: u16 = 0x0005;
pub const FEATURE_BATTERY_STATUS: u16 = 0x1000;
pub const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;
pub const FEATURE_REPROG_CONTROLS_V4: u16 = 0x1B04;

/// Whether a HID collection carries HID++ long reports: usage page 0xFF00
/// usage 2 behind receivers and over USB, 0xFF43/0x0202 over Bluetooth LE.
pub fn is_hidpp_collection(vid: u16, usage_page: u16, usage: u16) -> bool {
    vid == LOGITECH_VID && matches!((usage_page, usage), (0xFF00, 0x0002) | (0xFF43, 0x0202))
}

pub fn feature_name(id: u16) -> &'static str {
    match id {
        0x0000 => "ROOT",
        0x0001 => "FEATURE_SET",
        0x0002 => "FEATURE_INFO",
        0x0003 => "DEVICE_FW_VERSION",
        0x0004 => "DEVICE_UNIT_ID",
        0x0005 => "DEVICE_NAME",
        0x0006 => "DEVICE_GROUPS",
        0x0007 => "DEVICE_FRIENDLY_NAME",
        0x0008 => "KEEP_ALIVE",
        0x0020 => "CONFIG_CHANGE",
        0x0021 => "CRYPTO_ID",
        0x00C2 => "DFU_CONTROL_SIGNED",
        0x00D0 => "DFU",
        0x1000 => "BATTERY_STATUS",
        0x1001 => "BATTERY_VOLTAGE",
        0x1004 => "UNIFIED_BATTERY",
        0x1814 => "CHANGE_HOST",
        0x1815 => "HOSTS_INFO",
        0x1B00 => "REPROG_CONTROLS",
        0x1B04 => "REPROG_CONTROLS_V4",
        0x1D4B => "WIRELESS_DEVICE_STATUS",
        0x1DF3 => "EQUAD_DJ_DEBUG_INFO",
        0x1E00 => "ENABLE_HIDDEN_FEATURES",
        0x1F20 => "ADC_MEASUREMENT",
        0x2005 => "BUTTON_SWAP_CANCEL",
        0x2100 => "VERTICAL_SCROLLING",
        0x2121 => "HIRES_WHEEL",
        0x2201 => "ADJUSTABLE_DPI",
        0x2205 => "POINTER_SPEED",
        0x40A3 => "FN_INVERSION",
        _ => "?",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidppError {
    Io(String),
    Timeout,
    /// HID++ 2.0 error code.
    Protocol(u8),
    /// HID++ 1.0 error code (the device or index speaks HID++ 1.0 only).
    Legacy(u8),
}

impl fmt::Display for HidppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Timeout => f.write_str("no response"),
            Self::Protocol(c) => {
                let name = match c {
                    1 => "unknown",
                    2 => "invalid argument",
                    3 => "out of range",
                    4 => "hardware error",
                    5 => "internal",
                    6 => "invalid feature index",
                    7 => "invalid function",
                    8 => "busy",
                    9 => "unsupported",
                    _ => "?",
                };
                write!(f, "HID++ 2.0 error 0x{c:02x} ({name})")
            }
            Self::Legacy(c) => {
                let name = match c {
                    1 => "invalid sub-id: HID++ 1.0 only",
                    8 => "unknown device",
                    9 => "resource error / device asleep",
                    _ => "?",
                };
                write!(f, "HID++ 1.0 error 0x{c:02x} ({name})")
            }
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// An open HID++ collection.
pub struct Hidpp {
    device: HidDevice,
    pub path: String,
    /// Reports that arrived while waiting for a response (notifications).
    pub stray: Vec<Vec<u8>>,
}

impl Hidpp {
    pub fn open(api: &HidApi, path: &CStr) -> Result<Self, HidppError> {
        let device = api
            .open_path(path)
            .map_err(|e| HidppError::Io(e.to_string()))?;
        Ok(Self {
            device,
            path: path.to_string_lossy().into_owned(),
            stray: Vec::new(),
        })
    }

    pub fn open_str(api: &HidApi, path: &str) -> Result<Self, HidppError> {
        let path = CString::new(path).map_err(|e| HidppError::Io(e.to_string()))?;
        Self::open(api, &path)
    }

    /// Reads one report, or `None` after `timeout`.
    pub fn read(&self, timeout: Duration) -> Result<Option<Vec<u8>>, HidppError> {
        let mut buf = [0u8; 64];
        let n = self
            .device
            .read_timeout(&mut buf, timeout.as_millis().min(i32::MAX as u128) as i32)
            .map_err(|e| HidppError::Io(e.to_string()))?;
        Ok((n > 0).then(|| buf[..n].to_vec()))
    }

    /// Sends a long request and waits for its response parameters.
    pub fn request(
        &mut self,
        index: u8,
        feature: u8,
        function: u8,
        params: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, HidppError> {
        let mut report = [0u8; LONG_LEN];
        report[0] = REPORT_LONG;
        report[1] = index;
        report[2] = feature;
        report[3] = (function << 4) | SW_ID;
        let n = params.len().min(LONG_LEN - 4);
        report[4..4 + n].copy_from_slice(&params[..n]);
        self.device
            .write(&report)
            .map_err(|e| HidppError::Io(e.to_string()))?;

        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HidppError::Timeout);
            }
            let Some(r) = self.read(left)? else { continue };
            if r.len() >= 7 && r[1] == index {
                if r[0] == REPORT_LONG && r[2] == feature && r[3] == report[3] {
                    return Ok(r[4..].to_vec());
                }
                if r[2] == 0xFF && r[3] == feature && r[4] == report[3] {
                    return Err(HidppError::Protocol(r[5]));
                }
                if r[0] == REPORT_SHORT && r[2] == 0x8F && r[3] == feature {
                    return Err(HidppError::Legacy(r[5]));
                }
            }
            self.stray.push(r);
        }
    }

    /// Protocol version (major, minor) of the device at `index`.
    pub fn protocol_version(&mut self, index: u8) -> Result<(u8, u8), HidppError> {
        let r = self.request(index, 0, 1, &[0, 0, 0x5A], Duration::from_millis(500))?;
        Ok((r[0], r[1]))
    }

    /// Feature table index of `feature`, or `None` if unsupported.
    pub fn feature_index(&mut self, index: u8, feature: u16) -> Result<Option<u8>, HidppError> {
        let r = self.request(
            index,
            0,
            0,
            &feature.to_be_bytes(),
            Duration::from_millis(500),
        )?;
        Ok((r[0] != 0 || feature == FEATURE_ROOT).then_some(r[0]))
    }

    /// Every feature as (index, id, type, version).
    pub fn features(&mut self, index: u8) -> Result<Vec<(u8, u16, u8, u8)>, HidppError> {
        let Some(set) = self.feature_index(index, FEATURE_SET)? else {
            return Ok(Vec::new());
        };
        let count = self.request(index, set, 0, &[], Duration::from_millis(500))?[0];
        let mut out = vec![(0, FEATURE_ROOT, 0, 0)];
        for i in 1..=count {
            let r = self.request(index, set, 1, &[i], Duration::from_millis(500))?;
            out.push((i, u16::from_be_bytes([r[0], r[1]]), r[2], r[3]));
        }
        Ok(out)
    }

    pub fn device_name(&mut self, index: u8, feature: u8) -> Result<String, HidppError> {
        let len = self.request(index, feature, 0, &[], Duration::from_millis(500))?[0] as usize;
        let mut name = Vec::new();
        while name.len() < len {
            let r = self.request(
                index,
                feature,
                1,
                &[name.len() as u8],
                Duration::from_millis(500),
            )?;
            let chunk: Vec<u8> = r.into_iter().take_while(|&b| b != 0).collect();
            if chunk.is_empty() {
                break;
            }
            name.extend(chunk);
        }
        name.truncate(len);
        Ok(String::from_utf8_lossy(&name).into_owned())
    }

    /// Controls reported by REPROG_CONTROLS_V4 at feature index `feature`.
    pub fn controls(&mut self, index: u8, feature: u8) -> Result<Vec<Control>, HidppError> {
        let count = self.request(index, feature, 0, &[], Duration::from_millis(500))?[0];
        (0..count)
            .map(|i| {
                let r = self.request(index, feature, 1, &[i], Duration::from_millis(500))?;
                Ok(Control {
                    cid: u16::from_be_bytes([r[0], r[1]]),
                    tid: u16::from_be_bytes([r[2], r[3]]),
                    flags: r[4],
                    position: r[5],
                    group: r[6],
                    group_mask: r[7],
                    flags2: r[8],
                })
            })
            .collect()
    }

    /// Current reporting of a control as raw response bytes.
    pub fn control_reporting(
        &mut self,
        index: u8,
        feature: u8,
        cid: u16,
    ) -> Result<Vec<u8>, HidppError> {
        let r = self.request(
            index,
            feature,
            2,
            &cid.to_be_bytes(),
            Duration::from_millis(500),
        )?;
        Ok(r[..5].to_vec())
    }

    /// Temporarily diverts (`true`) or restores (`false`) a control.
    pub fn set_divert(
        &mut self,
        index: u8,
        feature: u8,
        cid: u16,
        divert: bool,
    ) -> Result<Vec<u8>, HidppError> {
        // Flags: bit 0 divert, bit 1 "divert bit is valid".
        let flags = if divert { 0x03 } else { 0x02 };
        let [hi, lo] = cid.to_be_bytes();
        let r = self.request(
            index,
            feature,
            3,
            &[hi, lo, flags, 0, 0],
            Duration::from_millis(500),
        )?;
        Ok(r[..5].to_vec())
    }
}

/// One entry of REPROG_CONTROLS_V4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    /// Control id (the physical button).
    pub cid: u16,
    /// Task id (what the button does by default).
    pub tid: u16,
    pub flags: u8,
    pub position: u8,
    pub group: u8,
    pub group_mask: u8,
    pub flags2: u8,
}

impl Control {
    pub fn divertable(&self) -> bool {
        self.flags & 0x20 != 0
    }

    pub fn describe_flags(&self) -> String {
        let names = [
            (0x01, "mouse"),
            (0x02, "fn"),
            (0x04, "nonstandard"),
            (0x08, "fn-toggle"),
            (0x10, "reprogrammable"),
            (0x20, "divertable"),
            (0x40, "persist-divertable"),
            (0x80, "virtual"),
        ];
        let set: Vec<&str> = names
            .iter()
            .filter(|(bit, _)| self.flags & bit != 0)
            .map(|(_, n)| *n)
            .collect();
        set.join(",")
    }
}

/// Pressed controls from a "diverted buttons" notification of
/// REPROG_CONTROLS_V4 at feature index `feature`; empty when all released.
pub fn diverted_buttons(report: &[u8], feature: u8) -> Option<Vec<u16>> {
    if report.len() < 12 || report[0] != REPORT_LONG || report[2] != feature || report[3] != 0 {
        return None;
    }
    Some(
        report[4..12]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .filter(|&cid| cid != 0)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidpp_collections_are_recognized() {
        assert!(is_hidpp_collection(0x046D, 0xFF00, 0x0002));
        assert!(is_hidpp_collection(0x046D, 0xFF43, 0x0202));
        assert!(!is_hidpp_collection(0x046D, 0xFF00, 0x0001));
        assert!(!is_hidpp_collection(0x1234, 0xFF00, 0x0002));
    }

    #[test]
    fn diverted_button_notifications_are_decoded() {
        let mut report = [0u8; 20];
        report[..8].copy_from_slice(&[0x11, 0x01, 0x05, 0x00, 0x00, 0xDA, 0x00, 0x00]);
        assert_eq!(diverted_buttons(&report, 5), Some(vec![0x00DA]));
        report[5] = 0;
        assert_eq!(diverted_buttons(&report, 5), Some(vec![]));
        // A response (non-zero function/software id) is not a notification.
        report[3] = 0x3A;
        assert_eq!(diverted_buttons(&report, 5), None);
    }
}
