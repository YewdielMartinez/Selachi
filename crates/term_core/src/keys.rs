//! Keyboard input encoding (xterm-style) into bytes for the PTY.

/// A key press described with gpui-style key names.
#[derive(Copy, Clone, Debug)]
pub struct KeyInput<'a> {
    /// Key name: `"enter"`, `"backspace"`, `"tab"`, `"escape"`, `"up"`, `"down"`, `"left"`,
    /// `"right"`, `"home"`, `"end"`, `"pageup"`, `"pagedown"`, `"delete"`, `"insert"`,
    /// `"f1"`..`"f12"`, `"space"`, or a single character such as `"a"`.
    pub key: &'a str,
    /// Text produced by the key press, if any (respects shift and layout).
    pub key_char: Option<&'a str>,
    /// Control modifier.
    pub ctrl: bool,
    /// Alt modifier.
    pub alt: bool,
    /// Shift modifier.
    pub shift: bool,
}

impl KeyInput<'_> {
    /// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    fn modifier_param(&self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn has_mods(&self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

/// Encodes a key press into the bytes a terminal application expects.
///
/// `app_cursor` selects SS3 (`ESC O x`) instead of CSI for unmodified arrows/home/end
/// (DECCKM). Returns `None` for keys that produce no input (bare modifiers, unknown names,
/// unmappable ctrl combinations).
pub fn encode_key(input: &KeyInput, app_cursor: bool) -> Option<Vec<u8>> {
    let m = input.modifier_param();

    // Cursor-like keys: CSI/SS3 + final letter.
    let letter = match input.key {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };
    if let Some(l) = letter {
        return Some(if input.has_mods() {
            format!("\x1b[1;{m}{}", l as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', l]
        } else {
            vec![0x1b, b'[', l]
        });
    }

    // Tilde keys: CSI n ~.
    let tilde = match input.key {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        return Some(if input.has_mods() {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        });
    }

    // F1-F4: SS3 P..S, or CSI 1;m P..S with modifiers.
    let ss3 = match input.key {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    };
    if let Some(l) = ss3 {
        return Some(if input.has_mods() {
            format!("\x1b[1;{m}{}", l as char).into_bytes()
        } else {
            vec![0x1b, b'O', l]
        });
    }

    // Single-byte named keys. Alt prefixes ESC.
    let named: Option<Vec<u8>> = match input.key {
        "enter" => Some(vec![b'\r']),
        "tab" if input.shift && !input.alt && !input.ctrl => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(vec![b'\t']),
        "escape" => Some(vec![0x1b]),
        "backspace" if input.ctrl => Some(vec![0x08]),
        "backspace" => Some(vec![0x7f]),
        "space" if input.ctrl => Some(vec![0x00]),
        "space" => Some(vec![b' ']),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(with_alt(bytes, input.alt));
    }

    // Everything else: a printable character.
    let base = single_char(input.key).or_else(|| input.key_char.and_then(single_char));

    if input.ctrl {
        let c = base?;
        let byte = ctrl_byte(c)?;
        return Some(with_alt(vec![byte], input.alt));
    }

    // Prefer the produced text; fall back to the key itself (uppercased under shift).
    let text: String = match input.key_char {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            let c = single_char(input.key)?;
            if input.shift {
                c.to_uppercase().collect()
            } else {
                c.to_string()
            }
        }
    };
    if text.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(with_alt(text.into_bytes(), input.alt))
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// Control-code for `ctrl+c` where `c` is a letter or one of the classic punctuation keys.
fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | '2' | ' ' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> KeyInput<'_> {
        KeyInput {
            key: k,
            key_char: None,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn enc(i: KeyInput) -> Vec<u8> {
        encode_key(&i, false).expect("encodes")
    }

    #[test]
    fn plain_keys() {
        assert_eq!(enc(key("enter")), b"\r");
        assert_eq!(enc(key("backspace")), vec![0x7f]);
        assert_eq!(enc(key("tab")), b"\t");
        assert_eq!(enc(key("escape")), vec![0x1b]);
        assert_eq!(enc(key("space")), b" ");
        assert_eq!(enc(key("delete")), b"\x1b[3~");
        assert_eq!(enc(key("pageup")), b"\x1b[5~");
        assert_eq!(enc(key("f1")), b"\x1bOP");
        assert_eq!(enc(key("f5")), b"\x1b[15~");
        assert_eq!(enc(key("f12")), b"\x1b[24~");
        assert!(encode_key(&key("shift"), false).is_none());
    }

    #[test]
    fn arrows_and_app_cursor() {
        assert_eq!(encode_key(&key("up"), false).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(&key("up"), true).unwrap(), b"\x1bOA");
        assert_eq!(encode_key(&key("home"), true).unwrap(), b"\x1bOH");
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("up")
            }),
            b"\x1b[1;5A"
        );
        assert_eq!(
            enc(KeyInput {
                shift: true,
                ..key("left")
            }),
            b"\x1b[1;2D"
        );
        assert_eq!(
            enc(KeyInput {
                alt: true,
                ctrl: true,
                ..key("right")
            }),
            b"\x1b[1;7C"
        );
        // Modifiers override application mode.
        assert_eq!(
            encode_key(
                &KeyInput {
                    ctrl: true,
                    ..key("down")
                },
                true
            )
            .unwrap(),
            b"\x1b[1;5B"
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("delete")
            }),
            b"\x1b[3;5~"
        );
        assert_eq!(
            enc(KeyInput {
                shift: true,
                ..key("f1")
            }),
            b"\x1b[1;2P"
        );
    }

    #[test]
    fn ctrl_alt_shift() {
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("a")
            }),
            vec![0x01]
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("c")
            }),
            vec![0x03]
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("z")
            }),
            vec![0x1a]
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                shift: true,
                ..key("a")
            }),
            vec![0x01]
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("[")
            }),
            vec![0x1b]
        );
        assert_eq!(
            enc(KeyInput {
                ctrl: true,
                ..key("space")
            }),
            vec![0x00]
        );
        assert!(
            encode_key(
                &KeyInput {
                    ctrl: true,
                    ..key("1")
                },
                false
            )
            .is_none()
        );
        assert_eq!(
            enc(KeyInput {
                alt: true,
                ..key("b")
            }),
            b"\x1bb"
        );
        assert_eq!(
            enc(KeyInput {
                alt: true,
                ctrl: true,
                ..key("a")
            }),
            b"\x1b\x01"
        );
        assert_eq!(
            enc(KeyInput {
                alt: true,
                ..key("backspace")
            }),
            b"\x1b\x7f"
        );
        assert_eq!(
            enc(KeyInput {
                shift: true,
                ..key("tab")
            }),
            b"\x1b[Z"
        );
        assert_eq!(
            enc(KeyInput {
                alt: true,
                ..key("enter")
            }),
            b"\x1b\r"
        );
    }

    #[test]
    fn printable_uses_key_char() {
        assert_eq!(
            enc(KeyInput {
                key_char: Some("A"),
                shift: true,
                ..key("a")
            }),
            b"A"
        );
        assert_eq!(
            enc(KeyInput {
                shift: true,
                ..key("a")
            }),
            b"A"
        );
        assert_eq!(
            enc(KeyInput {
                key_char: Some("!"),
                shift: true,
                ..key("1")
            }),
            b"!"
        );
        assert_eq!(
            enc(KeyInput {
                key_char: Some("\u{e9}"),
                ..key("e")
            }),
            "\u{e9}".as_bytes()
        );
        assert_eq!(
            enc(KeyInput {
                key_char: Some("x"),
                alt: true,
                ..key("x")
            }),
            b"\x1bx"
        );
        assert!(encode_key(&key("capslock"), false).is_none());
    }
}
