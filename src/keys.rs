//! Key names to PC/AT set-1 scancodes, and `ctrl+shift+esc`-style combo parsing.

use anyhow::{Result, bail};
use ironrdp::input::Scancode;

pub fn scancode(name: &str) -> Option<Scancode> {
    let name = name.to_ascii_lowercase();
    let plain = |code| Some(Scancode::from_u8(false, code));
    let ext = |code| Some(Scancode::from_u8(true, code));
    if let [c] = name.as_bytes() {
        const LETTERS: &[u8; 26] =
            b"\x1e\x30\x2e\x20\x12\x21\x22\x23\x17\x24\x25\x26\x32\x31\x18\x19\x10\x13\x1f\x14\x16\x2f\x11\x2d\x15\x2c";
        match c {
            b'a'..=b'z' => return plain(LETTERS[usize::from(c - b'a')]),
            b'1'..=b'9' => return plain(c - b'1' + 0x02),
            b'0' => return plain(0x0b),
            _ => {}
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return match n {
            1..=10 => plain(0x3a + n),
            11 => plain(0x57),
            12 => plain(0x58),
            _ => None,
        };
    }
    match name.as_str() {
        "enter" | "return" => plain(0x1c),
        "esc" | "escape" => plain(0x01),
        "backspace" => plain(0x0e),
        "tab" => plain(0x0f),
        "space" => plain(0x39),
        "ctrl" | "control" => plain(0x1d),
        "shift" => plain(0x2a),
        "alt" => plain(0x38),
        "win" | "super" | "meta" | "cmd" => ext(0x5b),
        "up" => ext(0x48),
        "down" => ext(0x50),
        "left" => ext(0x4b),
        "right" => ext(0x4d),
        "home" => ext(0x47),
        "end" => ext(0x4f),
        "pageup" => ext(0x49),
        "pagedown" => ext(0x51),
        "insert" => ext(0x52),
        "delete" | "del" => ext(0x53),
        _ => None,
    }
}

/// `"ctrl+shift+esc"` -> scancodes in press order.
pub fn parse_combo(combo: &str) -> Result<Vec<Scancode>> {
    combo
        .split('+')
        .map(|k| match scancode(k.trim()) {
            Some(code) => Ok(code),
            None => bail!("unknown key {k:?} in {combo:?}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combos() {
        let codes = parse_combo("Win+R").unwrap();
        assert_eq!(
            codes,
            vec![Scancode::from_u8(true, 0x5b), Scancode::from_u8(false, 0x13)]
        );
        assert_eq!(scancode("a"), Some(Scancode::from_u8(false, 0x1e)));
        assert_eq!(scancode("z"), Some(Scancode::from_u8(false, 0x2c)));
        assert_eq!(scancode("0"), Some(Scancode::from_u8(false, 0x0b)));
        assert_eq!(scancode("f12"), Some(Scancode::from_u8(false, 0x58)));
        assert!(parse_combo("ctrl+nope").is_err());
    }
}
