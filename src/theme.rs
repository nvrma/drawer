//! Palette: the terminal's own. Every colour is either the terminal default
//! (`Reset`) or one of its 16 ANSI slots, so drawer always matches the active
//! terminal / Omarchy theme and follows it live when the theme changes.

use ratatui::style::Color;

/// Screen, pane and popup grounds: the terminal's own background.
pub const OAK: Color = Color::Reset;
pub const PANEL: Color = Color::Reset;
pub const CARD: Color = Color::Reset;
/// Selected row: "bright black", the theme's own selection/comment grey.
pub const ROW_SEL: Color = Color::Indexed(8);

/// Primary text: the terminal's foreground.
pub const IVORY: Color = Color::Reset;
/// Secondary text: ANSI white (a step below the foreground in most themes).
/// Also the readable text colour on ROW_SEL.
pub const GRAIN: Color = Color::Indexed(7);
/// Tertiary text, always italic: ANSI bright black. Never drawn on ROW_SEL.
pub const FAINT: Color = Color::Indexed(8);

/// Pane borders.
pub const RULE: Color = Color::Indexed(8);
/// Focused pane's top rule (and popups, which always hold focus).
pub const RED_RULE: Color = Color::Indexed(4);
/// Only on what can be acted on right now: key hints, selected names.
pub const BRASS: Color = Color::Indexed(3);
/// Download stamps.
pub const STAMP: Color = Color::Indexed(5);
/// Text drawn on top of a coloured tab.
pub const ON_TAB: Color = Color::Indexed(0);

pub const OK: Color = Color::Indexed(2);
pub const ERR: Color = Color::Indexed(1);

/// One colour per machine, from the theme's accents (red is kept for errors).
const TABS: [Color; 10] = [
    Color::Indexed(4),
    Color::Indexed(5),
    Color::Indexed(6),
    Color::Indexed(2),
    Color::Indexed(3),
    Color::Indexed(12),
    Color::Indexed(13),
    Color::Indexed(14),
    Color::Indexed(10),
    Color::Indexed(11),
];

/// Stable tab colour for a machine alias (FNV-1a, so it never shifts when the
/// config is reordered).
pub fn tab_for(alias: &str) -> Color {
    let mut h: u32 = 0x811c_9dc5;
    for b in alias.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    TABS[(h % TABS.len() as u32) as usize]
}

/// Letter-spaced uppercase pane titles.
pub fn tracked(s: &str) -> String {
    let up = s.to_uppercase();
    let mut out = String::with_capacity(up.len() * 2);
    for (i, c) in up.chars().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Nerd Font glyphs, with a plain fallback when DRAWER_NO_ICONS is set.
pub struct Icons {
    pub folder: &'static str,
    pub file: &'static str,
    pub link: &'static str,
}

pub fn icons() -> Icons {
    if std::env::var_os("DRAWER_NO_ICONS").is_some() {
        Icons { folder: "/", file: " ", link: "@" }
    } else {
        Icons { folder: "\u{f07b}", file: "\u{f15b}", link: "\u{f0c1}" }
    }
}
