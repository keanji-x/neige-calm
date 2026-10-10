//! Viewer input: browser events mapped to window pixels and evdev codes.

use crate::protocol::ClientMessage;

/// Input for the streamed window. Coordinates are window pixels; codes are
/// Linux evdev codes (`BTN_LEFT` = 0x110, `KEY_A` = 30).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StreamInput {
    Pointer {
        x: f64,
        y: f64,
    },
    Button {
        code: u32,
        pressed: bool,
    },
    /// Scroll distance in window pixels; positive is right and down.
    Wheel {
        dx: f64,
        dy: f64,
    },
    Key {
        evdev: u32,
        pressed: bool,
    },
}

/// Why a client message produced no input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ignored {
    /// A `key` whose `code` has no entry in the US-layout table.
    UnknownKey(String),
    /// A `button` index with no evdev button.
    UnknownButton(i64),
    /// A pointer event before the window size is known.
    NoSize,
}

/// Maps one client message to input. Pointer coordinates are clamped into the
/// window (`0..=width-1`, `0..=height-1`); without a known size they are dropped.
pub(crate) fn translate(
    message: ClientMessage,
    size: Option<(u32, u32)>,
) -> Result<StreamInput, Ignored> {
    match message {
        ClientMessage::Pointer { x, y } => {
            let (width, height) = size.ok_or(Ignored::NoSize)?;
            Ok(StreamInput::Pointer {
                x: clamp(x, width),
                y: clamp(y, height),
            })
        }
        ClientMessage::Button { button, pressed } => evdev_button(button)
            .map(|code| StreamInput::Button { code, pressed })
            .ok_or(Ignored::UnknownButton(button)),
        ClientMessage::Wheel { dx, dy } => Ok(StreamInput::Wheel {
            dx: finite(dx),
            dy: finite(dy),
        }),
        ClientMessage::Key { code, pressed } => match evdev_key(&code) {
            Some(evdev) => Ok(StreamInput::Key { evdev, pressed }),
            None => Err(Ignored::UnknownKey(code)),
        },
    }
}

fn clamp(value: f64, side: u32) -> f64 {
    let max = f64::from(side.saturating_sub(1));
    finite(value).clamp(0.0, max)
}

fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// The evdev button for a browser `MouseEvent.button` index.
pub fn evdev_button(button: i64) -> Option<u32> {
    Some(match button {
        0 => 0x110, // BTN_LEFT
        1 => 0x112, // BTN_MIDDLE
        2 => 0x111, // BTN_RIGHT
        3 => 0x113, // BTN_SIDE (browser "back")
        4 => 0x114, // BTN_EXTRA (browser "forward")
        _ => return None,
    })
}

/// The evdev key for a browser `KeyboardEvent.code` on a US layout.
///
/// `code` names a physical key, so the table is layout independent on the
/// browser side; the compositor applies its US keymap to the evdev code.
pub fn evdev_key(code: &str) -> Option<u32> {
    if let Some(letter) = code.strip_prefix("Key") {
        return letter_key(letter);
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return digit_key(digit);
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return function_key(n);
    }
    Some(match code {
        "Escape" => 1,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "ControlLeft" => 29,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "ShiftLeft" => 42,
        "Backslash" => 43,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "ShiftRight" => 54,
        "NumpadMultiply" => 55,
        "AltLeft" => 56,
        "Space" => 57,
        "CapsLock" => 58,
        "NumLock" => 69,
        "ScrollLock" => 70,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadSubtract" => 74,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "NumpadAdd" => 78,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad0" => 82,
        "NumpadDecimal" => 83,
        "IntlBackslash" => 86,
        "NumpadEnter" => 96,
        "ControlRight" => 97,
        "NumpadDivide" => 98,
        "PrintScreen" => 99,
        "AltRight" => 100,
        "Home" => 102,
        "ArrowUp" => 103,
        "PageUp" => 104,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "End" => 107,
        "ArrowDown" => 108,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        "NumpadEqual" => 117,
        "Pause" => 119,
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ContextMenu" => 127,
        _ => return None,
    })
}

fn letter_key(letter: &str) -> Option<u32> {
    // evdev codes follow the physical QWERTY rows, not the alphabet.
    const ROWS: [(&str, u32); 3] = [("QWERTYUIOP", 16), ("ASDFGHJKL", 30), ("ZXCVBNM", 44)];
    let [c] = letter.as_bytes() else {
        return None;
    };
    ROWS.iter()
        .find_map(|(row, first)| row.bytes().position(|k| k == *c).map(|i| first + i as u32))
}

fn digit_key(digit: &str) -> Option<u32> {
    match digit.as_bytes() {
        [b'0'] => Some(11),
        [d @ b'1'..=b'9'] => Some(u32::from(d - b'1') + 2),
        _ => None,
    }
}

fn function_key(n: u32) -> Option<u32> {
    match n {
        1..=10 => Some(58 + n),   // KEY_F1 = 59
        11 | 12 => Some(76 + n),  // KEY_F11 = 87
        13..=24 => Some(170 + n), // KEY_F13 = 183
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_table_spot_checks() {
        let cases = [
            ("KeyA", 30),
            ("KeyQ", 16),
            ("KeyM", 50),
            ("KeyZ", 44),
            ("KeyP", 25),
            ("KeyL", 38),
            ("Digit1", 2),
            ("Digit9", 10),
            ("Digit0", 11),
            ("F1", 59),
            ("F10", 68),
            ("F11", 87),
            ("F12", 88),
            ("F13", 183),
            ("F24", 194),
            ("Escape", 1),
            ("Enter", 28),
            ("Space", 57),
            ("Backquote", 41),
            ("Slash", 53),
            ("ArrowLeft", 105),
            ("PageDown", 109),
            ("Delete", 111),
            ("ShiftLeft", 42),
            ("ControlRight", 97),
            ("AltRight", 100),
            ("MetaLeft", 125),
            ("Numpad0", 82),
            ("Numpad5", 76),
            ("NumpadEnter", 96),
            ("NumpadDivide", 98),
        ];
        for (code, evdev) in cases {
            assert_eq!(evdev_key(code), Some(evdev), "{code}");
        }
    }

    #[test]
    fn unknown_codes_have_no_key() {
        for code in [
            "", "Key", "KeyAA", "Keya", "Digit10", "F0", "F25", "Fn", "Lang1",
        ] {
            assert_eq!(evdev_key(code), None, "{code:?}");
        }
    }

    #[test]
    fn every_letter_maps_to_a_distinct_key() {
        let mut seen: Vec<u32> = ('A'..='Z')
            .map(|c| evdev_key(&format!("Key{c}")).expect("letter"))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 26);
    }

    #[test]
    fn buttons_map_to_evdev() {
        assert_eq!(evdev_button(0), Some(0x110));
        assert_eq!(evdev_button(1), Some(0x112));
        assert_eq!(evdev_button(2), Some(0x111));
        assert_eq!(evdev_button(5), None);
        assert_eq!(evdev_button(-1), None);
    }

    #[test]
    fn pointer_is_clamped_to_the_window() {
        let at = |x, y| translate(ClientMessage::Pointer { x, y }, Some((1280, 800)));
        assert_eq!(
            at(-5.0, 900.0),
            Ok(StreamInput::Pointer { x: 0.0, y: 799.0 })
        );
        assert_eq!(
            at(10.5, 20.0),
            Ok(StreamInput::Pointer { x: 10.5, y: 20.0 })
        );
        assert_eq!(
            translate(ClientMessage::Pointer { x: 1.0, y: 1.0 }, None),
            Err(Ignored::NoSize)
        );
    }
}
