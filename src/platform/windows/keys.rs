//! Virtual-key names and the mapping from remote keystrokes to buttons.
//!
//! Presentation remotes without vendor software behave like small keyboards.
//! Which keys a given model sends is recorded with `rustclick-diag`; the
//! table below covers the keys known from Logitech remotes in general.

pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_NEXT: u16 = 0x22;
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_B: u16 = 0x42;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_F5: u16 = 0x74;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RMENU: u16 = 0xA5;
pub const VK_OEM_PERIOD: u16 = 0xBE;

/// Human-readable name of a virtual key, for logs.
pub fn vk_name(vk: u16) -> String {
    let name = match vk {
        VK_TAB => "Tab",
        VK_RETURN => "Enter",
        VK_SHIFT => "Shift",
        VK_CONTROL => "Ctrl",
        VK_MENU => "Alt",
        VK_ESCAPE => "Esc",
        VK_SPACE => "Space",
        VK_PRIOR => "PageUp",
        VK_NEXT => "PageDown",
        VK_END => "End",
        VK_HOME => "Home",
        VK_LEFT => "Left",
        VK_UP => "Up",
        VK_RIGHT => "Right",
        VK_DOWN => "Down",
        VK_INSERT => "Insert",
        VK_DELETE => "Delete",
        VK_LWIN => "LWin",
        VK_RWIN => "RWin",
        VK_OEM_PERIOD => "Period",
        VK_LSHIFT..=VK_RMENU => "Modifier",
        0xAD => "VolumeMute",
        0xAE => "VolumeDown",
        0xAF => "VolumeUp",
        0xB0 => "MediaNext",
        0xB1 => "MediaPrev",
        0xB3 => "MediaPlayPause",
        0x30..=0x39 | 0x41..=0x5A => return char::from(vk as u8).to_string(),
        0x70..=0x87 => return format!("F{}", vk - 0x6F),
        _ => return format!("VK 0x{vk:02X}"),
    };
    name.to_owned()
}

/// Modifier keys never produce button events on their own.
pub fn is_modifier(vk: u16) -> bool {
    matches!(
        vk,
        VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN | VK_RWIN | VK_LSHIFT..=VK_RMENU
    )
}

/// Raw button name for a key a remote sent. It is normalized by
/// [`crate::core::input::normalize_button`]; unknown keys get a stable
/// `key_xx` identifier so they can still be mapped.
pub fn button_for_vk(vk: u16) -> String {
    match vk {
        VK_NEXT | VK_RIGHT | VK_DOWN => "forward".into(),
        VK_PRIOR | VK_LEFT | VK_UP => "back".into(),
        VK_F5 => "f5".into(),
        VK_ESCAPE => "escape".into(),
        VK_B | VK_OEM_PERIOD => "blank".into(),
        VK_TAB => "tab".into(),
        _ => format!("key_{vk:02x}"),
    }
}

/// Keys whose scan code needs the extended-key flag in window messages.
pub fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        VK_PRIOR
            | VK_NEXT
            | VK_END
            | VK_HOME
            | VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_INSERT
            | VK_DELETE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::input::normalize_button;

    #[test]
    fn remote_keys_map_to_standard_buttons() {
        let button = |vk| normalize_button(&button_for_vk(vk)).unwrap();
        assert_eq!(button(VK_NEXT).as_str(), "forward");
        assert_eq!(button(VK_PRIOR).as_str(), "back");
        assert_eq!(button(VK_F5).as_str(), "f5");
        assert_eq!(button(0x7B).as_str(), "key_7b");
        assert_eq!(vk_name(VK_NEXT), "PageDown");
        assert_eq!(vk_name(0x41), "A");
        assert_eq!(vk_name(0x7B), "F12");
        assert!(is_modifier(VK_LSHIFT) && !is_modifier(VK_NEXT));
    }
}
