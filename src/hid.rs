//! USB HID scancodes, modifier bits, and the key-name lookup tables used by
//! the NanoKVM WebSocket HID protocol.

/// Left-side modifier bits as they appear in byte 0 of a USB HID boot-keyboard
/// report. Right-side variants exist in the spec (Ctrl/Shift/Alt/Meta = bits
/// 4..7) but we don't emit them.
pub mod modifier {
    pub const CTRL_LEFT: u8 = 1;
    pub const SHIFT_LEFT: u8 = 2;
    pub const ALT_LEFT: u8 = 4;
    pub const META_LEFT: u8 = 8;
}

/// A resolved key: HID scancode plus whether Shift must be held to produce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInfo {
    pub code: u8,
    pub shift: bool,
}

/// Resolve a key name or single character to its HID scancode.
///
/// Single-character inputs are matched case-sensitively first (so `'A'` picks
/// the shifted variant); anything else is looked up by lowercased name.
pub fn get_key_info(key: &str) -> Option<KeyInfo> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if let Some(code) = shifted_char_code(c) {
            return Some(KeyInfo { code, shift: true });
        }
        if let Some(code) = unshifted_char_code(c) {
            return Some(KeyInfo { code, shift: false });
        }
    }
    let mut buf = [0u8; 32];
    let lower = ascii_lowercase_into(key, &mut buf)?;
    named_key_code(lower).map(|code| KeyInfo { code, shift: false })
}

/// Map a single character to `(scancode, modifier-bitmask)` for typing through
/// the WS HID interface. Returns `None` for characters that don't correspond to
/// a US-QWERTY key.
pub fn char_to_keycode(c: char) -> Option<(u8, u8)> {
    match c {
        ' ' => Some((named_key_code("space")?, 0)),
        '\n' => Some((named_key_code("enter")?, 0)),
        '\t' => Some((named_key_code("tab")?, 0)),
        _ => {
            if let Some(code) = shifted_char_code(c) {
                return Some((code, modifier::SHIFT_LEFT));
            }
            unshifted_char_code(c).map(|code| (code, 0))
        }
    }
}

/// HID scancodes for unshifted single characters (lowercase letters, digits,
/// unshifted punctuation, including the backtick).
fn unshifted_char_code(c: char) -> Option<u8> {
    Some(match c {
        'a' => 0x04, 'b' => 0x05, 'c' => 0x06, 'd' => 0x07, 'e' => 0x08, 'f' => 0x09,
        'g' => 0x0A, 'h' => 0x0B, 'i' => 0x0C, 'j' => 0x0D, 'k' => 0x0E, 'l' => 0x0F,
        'm' => 0x10, 'n' => 0x11, 'o' => 0x12, 'p' => 0x13, 'q' => 0x14, 'r' => 0x15,
        's' => 0x16, 't' => 0x17, 'u' => 0x18, 'v' => 0x19, 'w' => 0x1A, 'x' => 0x1B,
        'y' => 0x1C, 'z' => 0x1D,

        '1' => 0x1E, '2' => 0x1F, '3' => 0x20, '4' => 0x21, '5' => 0x22,
        '6' => 0x23, '7' => 0x24, '8' => 0x25, '9' => 0x26, '0' => 0x27,

        '-' => 0x2D, '=' => 0x2E,
        '[' => 0x2F, ']' => 0x30,
        '\\' => 0x31,
        ';' => 0x33, '\'' => 0x34,
        '`' => 0x35,
        ',' => 0x36, '.' => 0x37, '/' => 0x38,

        _ => return None,
    })
}

/// HID scancodes for shift-modified characters (uppercase letters and shifted
/// punctuation).
fn shifted_char_code(c: char) -> Option<u8> {
    Some(match c {
        'A' => 0x04, 'B' => 0x05, 'C' => 0x06, 'D' => 0x07, 'E' => 0x08, 'F' => 0x09,
        'G' => 0x0A, 'H' => 0x0B, 'I' => 0x0C, 'J' => 0x0D, 'K' => 0x0E, 'L' => 0x0F,
        'M' => 0x10, 'N' => 0x11, 'O' => 0x12, 'P' => 0x13, 'Q' => 0x14, 'R' => 0x15,
        'S' => 0x16, 'T' => 0x17, 'U' => 0x18, 'V' => 0x19, 'W' => 0x1A, 'X' => 0x1B,
        'Y' => 0x1C, 'Z' => 0x1D,

        '!' => 0x1E, '@' => 0x1F, '#' => 0x20, '$' => 0x21, '%' => 0x22,
        '^' => 0x23, '&' => 0x24, '*' => 0x25, '(' => 0x26, ')' => 0x27,
        '_' => 0x2D, '+' => 0x2E,
        '{' => 0x2F, '}' => 0x30, '|' => 0x31,
        ':' => 0x33, '"' => 0x34, '~' => 0x35,
        '<' => 0x36, '>' => 0x37, '?' => 0x38,

        _ => return None,
    })
}

/// HID scancodes for multi-character key names (lowercased).
fn named_key_code(name: &str) -> Option<u8> {
    Some(match name {
        // Single-char aliases that also work via this path when fed a name.
        "enter" | "return" => 0x28,
        "escape" | "esc" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,

        // Function keys.
        "f1" => 0x3A, "f2" => 0x3B, "f3" => 0x3C, "f4" => 0x3D,
        "f5" => 0x3E, "f6" => 0x3F, "f7" => 0x40, "f8" => 0x41,
        "f9" => 0x42, "f10" => 0x43, "f11" => 0x44, "f12" => 0x45,

        // Control / navigation cluster.
        "printscreen" => 0x46, "scrolllock" => 0x47, "pause" => 0x48,
        "insert" => 0x49, "home" => 0x4A, "pageup" => 0x4B,
        "delete" => 0x4C, "end" => 0x4D, "pagedown" => 0x4E,

        // Arrows.
        "right" => 0x4F, "left" => 0x50, "down" => 0x51, "up" => 0x52,

        // Numpad.
        "numlock" => 0x53,
        "kp_divide" => 0x54, "kp_multiply" => 0x55, "kp_minus" => 0x56,
        "kp_plus" => 0x57, "kp_enter" => 0x58,
        "kp_1" => 0x59, "kp_2" => 0x5A, "kp_3" => 0x5B, "kp_4" => 0x5C,
        "kp_5" => 0x5D, "kp_6" => 0x5E, "kp_7" => 0x5F, "kp_8" => 0x60,
        "kp_9" => 0x61, "kp_0" => 0x62, "kp_decimal" => 0x63,

        // Modifier keys themselves (rarely used directly — included for parity).
        "capslock" => 0x39,
        "ctrl" | "lctrl" => 0xE0,
        "rctrl" => 0xE4,
        "shift" | "lshift" => 0xE1,
        "rshift" => 0xE5,
        "alt" | "lalt" => 0xE2,
        "ralt" => 0xE6,
        "meta" | "lmeta" | "win" | "cmd" | "super" => 0xE3,
        "rmeta" => 0xE7,

        _ => return None,
    })
}

/// Lowercase ASCII letters of `s` into `buf`, returning the slice as `&str`.
/// Returns `None` if `s` doesn't fit (silent fallback — no named key is that
/// long).
fn ascii_lowercase_into<'a>(s: &str, buf: &'a mut [u8]) -> Option<&'a str> {
    if s.len() > buf.len() {
        return None;
    }
    for (i, b) in s.bytes().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    // Safe: lowercasing ASCII preserves UTF-8 boundaries; if `s` had any
    // non-ASCII bytes they're passed through unchanged and remain valid.
    std::str::from_utf8(&buf[..s.len()]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_letter_resolves_unshifted() {
        let k = get_key_info("a").unwrap();
        assert_eq!(k, KeyInfo { code: 0x04, shift: false });
    }

    #[test]
    fn uppercase_letter_resolves_shifted() {
        let k = get_key_info("A").unwrap();
        assert_eq!(k, KeyInfo { code: 0x04, shift: true });
    }

    #[test]
    fn named_key_is_case_insensitive() {
        assert_eq!(get_key_info("enter").unwrap().code, 0x28);
        assert_eq!(get_key_info("ENTER").unwrap().code, 0x28);
        assert_eq!(get_key_info("Enter").unwrap().code, 0x28);
    }

    #[test]
    fn function_keys() {
        assert_eq!(get_key_info("f1").unwrap().code, 0x3A);
        assert_eq!(get_key_info("F12").unwrap().code, 0x45);
    }

    #[test]
    fn shifted_punctuation() {
        let k = get_key_info("!").unwrap();
        assert_eq!(k, KeyInfo { code: 0x1E, shift: true });
    }

    #[test]
    fn unshifted_punctuation() {
        let k = get_key_info("/").unwrap();
        assert_eq!(k, KeyInfo { code: 0x38, shift: false });
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(get_key_info("notakey").is_none());
        assert!(get_key_info("ñ").is_none());
    }

    #[test]
    fn char_whitespace_specials() {
        assert_eq!(char_to_keycode(' '), Some((0x2C, 0)));
        assert_eq!(char_to_keycode('\n'), Some((0x28, 0)));
        assert_eq!(char_to_keycode('\t'), Some((0x2B, 0)));
    }

    #[test]
    fn char_uppercase_emits_shift_modifier() {
        assert_eq!(char_to_keycode('A'), Some((0x04, modifier::SHIFT_LEFT)));
        assert_eq!(char_to_keycode('?'), Some((0x38, modifier::SHIFT_LEFT)));
    }

    #[test]
    fn char_unmappable() {
        assert_eq!(char_to_keycode('ñ'), None);
    }
}
