//! Keys for `window_input`: chords (`"ctrl+l"`) and US-ASCII text, mapped to
//! browser `KeyboardEvent.code` names on a US layout and from there to evdev
//! through `window-stream`'s table, the one code → evdev table.

use compositor::InputEvent;

/// Longest `text` one event may carry.
pub const MAX_TEXT: usize = 1000;

/// Why a key or text event cannot be typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError(pub String);

/// Presses and releases one chord: modifiers in order, then the key, then
/// everything released in reverse. Modifiers are `ctrl`, `shift`, `alt` and
/// `meta`; the key is a name (`enter`, `pagedown`, `f5`, `plus`, …) or one
/// printable character. Letters are case-insensitive (`shift+a` types `A`);
/// a symbol that needs Shift on a US layout (`?`) gets it.
pub fn chord(keys: &str) -> Result<Vec<InputEvent>, KeyError> {
    let bad = |why: String| KeyError(format!("key `{keys}`: {why}"));
    let mut parts: Vec<&str> = keys.split('+').collect();
    // `+` is the plus key, and `ctrl++` is Ctrl and the plus key.
    if keys == "+" {
        parts = vec!["+"];
    } else if keys.ends_with("++") {
        parts.truncate(parts.len() - 2);
        parts.push("+");
    }
    let Some((key, modifiers)) = parts.split_last() else {
        return Err(bad("empty".into()));
    };
    let mut held: Vec<&'static str> = Vec::new();
    for name in modifiers {
        let code = modifier(name).ok_or_else(|| {
            bad(format!(
                "`{name}` is not a modifier (ctrl, shift, alt, meta)"
            ))
        })?;
        if !held.contains(&code) {
            held.push(code);
        }
    }
    let (code, shifted) = key_code(key).ok_or_else(|| {
        bad(format!(
            "unknown key `{key}`; use a printable character or one of {NAMED_LIST}"
        ))
    })?;
    if shifted && !held.contains(&SHIFT) {
        held.push(SHIFT);
    }
    let mut codes = held;
    codes.push(code);
    press_release(&codes).map_err(bad)
}

/// Types `text` key by key. Printable US-ASCII, newline and tab only.
pub fn text(text: &str) -> Result<Vec<InputEvent>, KeyError> {
    if text.chars().count() > MAX_TEXT {
        return Err(KeyError(format!(
            "text is longer than {MAX_TEXT} characters; send it in several events"
        )));
    }
    if let Some(c) = text.chars().find(|c| char_code(*c).is_none()) {
        return Err(KeyError(format!(
            "text may hold printable US-ASCII, newline and tab only, not {c:?}: \
             non-ASCII input and IME are a known gap; to open a URL use page_open"
        )));
    }
    let mut events = Vec::new();
    for c in text.chars() {
        let (code, shifted) = char_code(c).expect("checked above");
        let codes: &[&str] = if shifted { &[SHIFT, code] } else { &[code] };
        events.extend(press_release(codes).map_err(KeyError)?);
    }
    Ok(events)
}

const SHIFT: &str = "ShiftLeft";

fn press_release(codes: &[&str]) -> Result<Vec<InputEvent>, String> {
    let evdev = codes
        .iter()
        .map(|code| {
            window_stream::evdev_key(code).ok_or_else(|| format!("no evdev key for `{code}`"))
        })
        .collect::<Result<Vec<u32>, String>>()?;
    let press = evdev.iter().map(|&evdev| InputEvent::Key {
        evdev,
        pressed: true,
    });
    let release = evdev.iter().rev().map(|&evdev| InputEvent::Key {
        evdev,
        pressed: false,
    });
    Ok(press.chain(release).collect())
}

fn modifier(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "ControlLeft",
        "shift" => SHIFT,
        "alt" => "AltLeft",
        "meta" | "super" => "MetaLeft",
        _ => return None,
    })
}

const NAMED_LIST: &str = "enter, tab, space, backspace, delete, insert, escape, home, end, \
     pageup, pagedown, up, down, left, right, plus, f1…f24";

/// A chord's key: a name, or one printable character (letters case-insensitive).
fn key_code(key: &str) -> Option<(&'static str, bool)> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return char_code(c.to_ascii_lowercase()).filter(|_| c != '\n' && c != '\t');
    }
    let lower = key.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Some((FUNCTION_KEYS[usize::from(n) - 1], false));
    }
    let code = match lower.as_str() {
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "space" => "Space",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" => "Insert",
        "escape" | "esc" => "Escape",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "plus" => return Some(("Equal", true)),
        _ => return None,
    };
    Some((code, false))
}

const FUNCTION_KEYS: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

const LETTER_CODES: [&str; 26] = [
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL",
    "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX",
    "KeyY", "KeyZ",
];
const DIGIT_CODES: [&str; 10] = [
    "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
    "Digit9",
];
/// The characters Shift gives on the digit row, `)` on Digit0 to `(` on Digit9.
const SHIFTED_DIGITS: &str = ")!@#$%^&*(";

/// The physical key, and whether Shift is held, that types `c` on a US layout.
fn char_code(c: char) -> Option<(&'static str, bool)> {
    if c.is_ascii_lowercase() {
        return Some((LETTER_CODES[(c as u8 - b'a') as usize], false));
    }
    if c.is_ascii_uppercase() {
        return Some((LETTER_CODES[(c as u8 - b'A') as usize], true));
    }
    if c.is_ascii_digit() {
        return Some((DIGIT_CODES[(c as u8 - b'0') as usize], false));
    }
    if let Some(i) = SHIFTED_DIGITS.find(c) {
        return Some((DIGIT_CODES[i], true));
    }
    Some(match c {
        ' ' => ("Space", false),
        '\n' => ("Enter", false),
        '\t' => ("Tab", false),
        '-' => ("Minus", false),
        '_' => ("Minus", true),
        '=' => ("Equal", false),
        '+' => ("Equal", true),
        '[' => ("BracketLeft", false),
        '{' => ("BracketLeft", true),
        ']' => ("BracketRight", false),
        '}' => ("BracketRight", true),
        '\\' => ("Backslash", false),
        '|' => ("Backslash", true),
        ';' => ("Semicolon", false),
        ':' => ("Semicolon", true),
        '\'' => ("Quote", false),
        '"' => ("Quote", true),
        '`' => ("Backquote", false),
        '~' => ("Backquote", true),
        ',' => ("Comma", false),
        '<' => ("Comma", true),
        '.' => ("Period", false),
        '>' => ("Period", true),
        '/' => ("Slash", false),
        '?' => ("Slash", true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // evdev codes from linux/input-event-codes.h.
    const KEY_TAB: u32 = 15;
    const KEY_L: u32 = 38;
    const KEY_A: u32 = 30;
    const KEY_1: u32 = 2;
    const KEY_SLASH: u32 = 53;
    const KEY_ENTER: u32 = 28;
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_EQUAL: u32 = 13;
    const KEY_PAGEDOWN: u32 = 109;
    const KEY_F5: u32 = 63;

    fn keys(events: &[InputEvent]) -> Vec<(u32, bool)> {
        events
            .iter()
            .map(|e| match e {
                InputEvent::Key { evdev, pressed } => (*evdev, *pressed),
                other => panic!("not a key: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_chord_presses_modifiers_then_the_key_and_releases_in_reverse() {
        assert_eq!(
            keys(&chord("ctrl+l").unwrap()),
            [
                (KEY_LEFTCTRL, true),
                (KEY_L, true),
                (KEY_L, false),
                (KEY_LEFTCTRL, false)
            ]
        );
        assert_eq!(
            keys(&chord("Ctrl+Shift+Tab").unwrap()),
            [
                (KEY_LEFTCTRL, true),
                (KEY_LEFTSHIFT, true),
                (KEY_TAB, true),
                (KEY_TAB, false),
                (KEY_LEFTSHIFT, false),
                (KEY_LEFTCTRL, false)
            ]
        );
        assert_eq!(
            keys(&chord("enter").unwrap()),
            [(KEY_ENTER, true), (KEY_ENTER, false)]
        );
        assert_eq!(keys(&chord("pagedown").unwrap())[0], (KEY_PAGEDOWN, true));
        assert_eq!(keys(&chord("F5").unwrap())[0], (KEY_F5, true));
        // Letters are case-insensitive; a shifted symbol brings its Shift.
        assert_eq!(keys(&chord("ctrl+L").unwrap())[1], (KEY_L, true));
        assert_eq!(
            keys(&chord("?").unwrap()),
            [
                (KEY_LEFTSHIFT, true),
                (KEY_SLASH, true),
                (KEY_SLASH, false),
                (KEY_LEFTSHIFT, false)
            ]
        );
        assert_eq!(
            keys(&chord("+").unwrap()),
            [
                (KEY_LEFTSHIFT, true),
                (KEY_EQUAL, true),
                (KEY_EQUAL, false),
                (KEY_LEFTSHIFT, false)
            ],
            "a bare + is the plus key"
        );
        for plus in ["ctrl++", "ctrl+plus"] {
            assert_eq!(
                keys(&chord(plus).unwrap()),
                [
                    (KEY_LEFTCTRL, true),
                    (KEY_LEFTSHIFT, true),
                    (KEY_EQUAL, true),
                    (KEY_EQUAL, false),
                    (KEY_LEFTSHIFT, false),
                    (KEY_LEFTCTRL, false)
                ]
            );
        }
    }

    #[test]
    fn a_bad_chord_names_the_problem() {
        for (keys, says) in [
            ("", "unknown key"),
            ("ctrl+", "unknown key"),
            ("hyper+l", "not a modifier"),
            ("ctrl+nokey", "unknown key"),
            ("f25", "unknown key"),
            ("é", "unknown key"),
        ] {
            let err = chord(keys).unwrap_err().0;
            assert!(err.contains(says), "{keys:?}: {err}");
        }
    }

    #[test]
    fn text_types_ascii_with_shift_where_the_us_layout_needs_it() {
        assert_eq!(
            keys(&text("aA!").unwrap()),
            [
                (KEY_A, true),
                (KEY_A, false),
                (KEY_LEFTSHIFT, true),
                (KEY_A, true),
                (KEY_A, false),
                (KEY_LEFTSHIFT, false),
                (KEY_LEFTSHIFT, true),
                (KEY_1, true),
                (KEY_1, false),
                (KEY_LEFTSHIFT, false)
            ]
        );
        // Every printable ASCII character, newline and tab maps to a key.
        let all: String = (0x20u8..0x7f).map(char::from).chain(['\n', '\t']).collect();
        assert_eq!(text(&all).unwrap().len(), 2 * all.len() + 2 * 47);
    }

    #[test]
    fn non_ascii_text_is_refused_and_names_the_known_gap() {
        for bad in ["héllo", "日本", "a\u{7f}", "\r", "\u{1F600}"] {
            let err = text(bad).unwrap_err().0;
            assert!(err.contains("page_open"), "{bad:?}: {err}");
            assert!(err.contains("IME"), "{bad:?}: {err}");
        }
        assert!(text(&"a".repeat(MAX_TEXT)).is_ok());
        assert!(text(&"a".repeat(MAX_TEXT + 1)).is_err());
    }
}
