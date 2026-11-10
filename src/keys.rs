//! Translate crossterm key events into the bytes a terminal would send.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(m: KeyModifiers) -> u8 {
    let mut p = 1;
    if m.contains(KeyModifiers::SHIFT) {
        p += 1;
    }
    if m.contains(KeyModifiers::ALT) {
        p += 2;
    }
    if m.contains(KeyModifiers::CONTROL) {
        p += 4;
    }
    p
}

fn csi_letter(letter: char, m: KeyModifiers, app_cursor: bool) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 {
        format!("\x1b[1;{p}{letter}").into_bytes()
    } else if app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn csi_tilde(n: u8, m: KeyModifiers) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 { format!("\x1b[{n};{p}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
}

pub fn encode(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let alt = m.contains(KeyModifiers::ALT);
    let mut bytes = match key.code {
        KeyCode::Char(c) if m.contains(KeyModifiers::CONTROL) => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                '@' | ' ' | '2' => 0,
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '-' | '7' => 31,
                '?' | '8' => 127,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => {
            if m.contains(KeyModifiers::CONTROL) {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        // Arrows carry Alt/Ctrl/Shift in the xterm modifier parameter.
        KeyCode::Up => return Some(csi_letter('A', m, app_cursor)),
        KeyCode::Down => return Some(csi_letter('B', m, app_cursor)),
        KeyCode::Right => return Some(csi_letter('C', m, app_cursor)),
        KeyCode::Left => return Some(csi_letter('D', m, app_cursor)),
        KeyCode::Home => return Some(csi_letter('H', m, app_cursor)),
        KeyCode::End => return Some(csi_letter('F', m, app_cursor)),
        KeyCode::PageUp => return Some(csi_tilde(5, m)),
        KeyCode::PageDown => return Some(csi_tilde(6, m)),
        KeyCode::Insert => return Some(csi_tilde(2, m)),
        KeyCode::Delete => return Some(csi_tilde(3, m)),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + n - 1) as char;
            let p = modifier_param(m);
            return Some(if p > 1 {
                format!("\x1b[1;{p}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            });
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            return Some(csi_tilde(code, m));
        }
        _ => return None,
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn basics() {
        assert_eq!(encode(k(KeyCode::Char('a'), KeyModifiers::NONE), false).unwrap(), b"a");
        assert_eq!(encode(k(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(), [3]);
        assert_eq!(encode(k(KeyCode::Char('b'), KeyModifiers::ALT), false).unwrap(), b"\x1bb");
        assert_eq!(encode(k(KeyCode::Enter, KeyModifiers::NONE), false).unwrap(), b"\r");
        assert_eq!(encode(k(KeyCode::Backspace, KeyModifiers::NONE), false).unwrap(), [0x7f]);
    }

    #[test]
    fn cursor_modes() {
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(k(KeyCode::Up, KeyModifiers::NONE), true).unwrap(), b"\x1bOA");
        assert_eq!(encode(k(KeyCode::Left, KeyModifiers::CONTROL), true).unwrap(), b"\x1b[1;5D");
        assert_eq!(encode(k(KeyCode::Delete, KeyModifiers::NONE), false).unwrap(), b"\x1b[3~");
        assert_eq!(encode(k(KeyCode::F(5), KeyModifiers::NONE), false).unwrap(), b"\x1b[15~");
        assert_eq!(encode(k(KeyCode::F(1), KeyModifiers::NONE), false).unwrap(), b"\x1bOP");
    }

    #[test]
    fn unicode() {
        assert_eq!(encode(k(KeyCode::Char('é'), KeyModifiers::NONE), false).unwrap(), "é".as_bytes());
    }
}
