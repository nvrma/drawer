//! Mouse support: reporting clicks to remote programs that asked for them
//! (htop, vim with `mouse=a`, tmux), double/triple-click detection, and the
//! shell pane's own text selection.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Lines moved per wheel notch in the scrollback.
pub const WHEEL_LINES: usize = 3;

/// The bytes a terminal would send a program for this mouse event, or None
/// when the program's mouse mode doesn't ask for it. `col` and `row` are
/// 0-based cells inside the shell pane.
pub fn encode(
    kind: MouseEventKind,
    mods: KeyModifiers,
    col: u16,
    row: u16,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use MouseEventKind as K;
    use MouseProtocolMode as M;
    let (mut code, release, motion): (u32, bool, bool) = match kind {
        K::Down(b) => (button(b), false, false),
        K::Up(b) => (button(b), true, false),
        K::Drag(b) => (button(b), false, true),
        K::Moved => (3, false, true),
        K::ScrollUp => (64, false, false),
        K::ScrollDown => (65, false, false),
        K::ScrollLeft => (66, false, false),
        K::ScrollRight => (67, false, false),
    };
    let wanted = match mode {
        M::None => false,
        M::Press => !release && !motion,
        M::PressRelease => !motion,
        M::ButtonMotion => kind != K::Moved,
        M::AnyMotion => true,
    };
    if !wanted {
        return None;
    }
    // X10 mode reports bare buttons; the others add modifier bits.
    if mode != M::Press {
        if mods.contains(KeyModifiers::SHIFT) {
            code += 4;
        }
        if mods.contains(KeyModifiers::ALT) {
            code += 8;
        }
        if mods.contains(KeyModifiers::CONTROL) {
            code += 16;
        }
    }
    if motion {
        code += 32;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match encoding {
        MouseProtocolEncoding::Sgr => {
            Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        }
        legacy => {
            // Legacy encodings can't say which button was released.
            if release {
                code = (code & !3) | 3;
            }
            let mut out = b"\x1b[M".to_vec();
            for v in [code, x, y] {
                let v = v + 32;
                match legacy {
                    MouseProtocolEncoding::Utf8 if v < 0x800 => {
                        let mut b = [0u8; 4];
                        out.extend(char::from_u32(v)?.encode_utf8(&mut b).as_bytes());
                    }
                    _ if v <= 255 => out.push(v as u8),
                    _ => return None, // too far right or down to encode
                }
            }
            Some(out)
        }
    }
}

fn button(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Counts quick successive clicks on the same cell: 1, 2 (word), 3 (line).
#[derive(Default)]
pub struct Clicks {
    last: Option<(Instant, u16, u16)>,
    count: u8,
}

impl Clicks {
    pub fn press(&mut self, col: u16, row: u16) -> u8 {
        let now = Instant::now();
        let again = self
            .last
            .is_some_and(|(t, c, r)| now.duration_since(t) < Duration::from_millis(400) && c == col && r == row);
        self.count = if again { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, col, row));
        self.count
    }
}

/// A text selection in the shell pane, in screen cells (row, col) of the
/// current view. `anchor` is where the drag started; `head` follows the
/// pointer. Both ends are inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The session the selection was made in.
    pub alias: String,
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    pub dragging: bool,
}

impl Selection {
    /// Start and end in reading order.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (s, e) = self.ordered();
        (row, col) >= s && (row, col) <= e
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selected text, with trailing blanks trimmed from every line.
    pub fn text(&self, screen: &vt100::Screen) -> String {
        let ((r1, c1), (r2, c2)) = self.ordered();
        let raw = screen.contents_between(r1, c1, r2, c2.saturating_add(1));
        let mut out: Vec<&str> = raw.split('\n').map(str::trim_end).collect();
        while out.last().is_some_and(|l| l.is_empty()) && out.len() > 1 {
            out.pop();
        }
        out.join("\n")
    }
}

/// Characters that end a double-click word. Paths, URLs, IPs and
/// `user@host:port` stay whole.
fn breaks_word(s: &str) -> bool {
    s.is_empty() || s.chars().any(|c| c.is_whitespace() || "\"'`()[]{}<>|;,".contains(c))
}

/// Column span of the word under (row, col), inclusive, or None on a blank.
pub fn word_at(screen: &vt100::Screen, row: u16, col: u16) -> Option<(u16, u16)> {
    let (_, cols) = screen.size();
    let at = |c: u16| screen.cell(row, c).map(|cell| cell.contents().to_string()).unwrap_or_default();
    let is_word = |c: u16| {
        let cell = screen.cell(row, c);
        // The right half of a wide character belongs to its left half.
        if cell.is_some_and(|x| x.is_wide_continuation()) {
            return c > 0 && !breaks_word(&at(c - 1));
        }
        !breaks_word(&at(c))
    };
    if col >= cols || !is_word(col) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cols && is_word(end + 1) {
        end += 1;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseEventKind as K;

    const N: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn sgr_reports_press_release_and_wheel() {
        let (m, e) = (MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr);
        assert_eq!(encode(K::Down(MouseButton::Left), N, 0, 0, m, e).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(encode(K::Up(MouseButton::Left), N, 4, 2, m, e).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(encode(K::ScrollUp, N, 9, 9, m, e).unwrap(), b"\x1b[<64;10;10M");
        assert_eq!(encode(K::Down(MouseButton::Right), KeyModifiers::CONTROL, 0, 0, m, e).unwrap(), b"\x1b[<18;1;1M");
        // Motion isn't wanted in press/release mode.
        assert!(encode(K::Drag(MouseButton::Left), N, 1, 1, m, e).is_none());
    }

    #[test]
    fn modes_filter_events() {
        let e = MouseProtocolEncoding::Sgr;
        assert!(encode(K::Down(MouseButton::Left), N, 0, 0, MouseProtocolMode::None, e).is_none());
        assert!(encode(K::Up(MouseButton::Left), N, 0, 0, MouseProtocolMode::Press, e).is_none());
        let drag = encode(K::Drag(MouseButton::Left), N, 2, 3, MouseProtocolMode::ButtonMotion, e).unwrap();
        assert_eq!(drag, b"\x1b[<32;3;4M");
        assert!(encode(K::Moved, N, 0, 0, MouseProtocolMode::ButtonMotion, e).is_none());
        assert_eq!(encode(K::Moved, N, 0, 0, MouseProtocolMode::AnyMotion, e).unwrap(), b"\x1b[<35;1;1M");
    }

    #[test]
    fn legacy_encoding() {
        let (m, e) = (MouseProtocolMode::PressRelease, MouseProtocolEncoding::Default);
        assert_eq!(encode(K::Down(MouseButton::Left), N, 0, 0, m, e).unwrap(), [0x1b, b'[', b'M', 32, 33, 33]);
        // Release is button 3 in the legacy encoding.
        assert_eq!(encode(K::Up(MouseButton::Left), N, 1, 1, m, e).unwrap(), [0x1b, b'[', b'M', 35, 34, 34]);
        // Past column 223 the legacy encoding has no way to say where.
        assert!(encode(K::Down(MouseButton::Left), N, 230, 0, m, e).is_none());
        let utf8 = encode(K::Down(MouseButton::Left), N, 230, 0, m, MouseProtocolEncoding::Utf8).unwrap();
        assert_eq!(String::from_utf8(utf8[3..].to_vec()).unwrap().chars().nth(1), Some('\u{107}'));
    }

    #[test]
    fn clicks_count_up_to_three() {
        let mut c = Clicks::default();
        assert_eq!(c.press(1, 1), 1);
        assert_eq!(c.press(1, 1), 2);
        assert_eq!(c.press(1, 1), 3);
        assert_eq!(c.press(1, 1), 1);
        assert_eq!(c.press(5, 1), 1);
    }

    #[test]
    fn selection_text_and_words() {
        let mut p = vt100::Parser::new(4, 40, 0);
        p.process(b"nv@pi:~$ ls /etc/nginx   \r\nsites-enabled  (conf)\r\n");
        let s = p.screen();
        let sel = Selection { alias: "pi".into(), anchor: (1, 4), head: (0, 3), dragging: false };
        assert_eq!(sel.text(s), "pi:~$ ls /etc/nginx\nsites");
        assert_eq!(word_at(s, 0, 14), Some((12, 21))); // /etc/nginx
        assert_eq!(word_at(s, 0, 2), Some((0, 7))); // nv@pi:~$
        assert_eq!(word_at(s, 1, 17), Some((16, 19))); // conf, without the brackets
        assert_eq!(word_at(s, 0, 30), None);
        assert!(sel.contains(0, 39) && sel.contains(1, 0) && !sel.contains(1, 5));
    }
}
