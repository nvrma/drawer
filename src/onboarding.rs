//! First-run welcome: three steps drawn over the whole screen.
//! Shown once (marker file in the XDG config dir); `drawer --welcome` or
//! `w` in the help screen shows it again.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Widget};

use crate::download;
use crate::hosts::Host;
use crate::theme::*;

pub const STEPS: usize = 3;

pub struct Onboarding {
    pub step: usize,
    pub setup: Setup,
}

/// What we could find out about this machine, for the "your setup" step.
pub struct Setup {
    pub ssh_version: Option<String>,
    pub pubkey: Option<PathBuf>,
    pub downloads: PathBuf,
}

impl Setup {
    pub fn detect(downloads: PathBuf) -> Self {
        let ssh_version = std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(if o.stderr.is_empty() { &o.stdout } else { &o.stderr }).trim().to_string()
            })
            .and_then(|v| v.split([',', ' ']).next().map(str::to_string))
            .filter(|v| !v.is_empty());
        let ssh = crate::hosts::ssh_dir();
        let pubkey = ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"].iter().map(|n| ssh.join(n)).find(|p| p.is_file());
        Self { ssh_version, pubkey, downloads }
    }
}

fn marker() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("drawer").join("welcomed"))
}

pub fn should_show() -> bool {
    marker().is_some_and(|m| !m.exists())
}

pub fn mark_done() {
    if let Some(m) = marker() {
        if let Some(dir) = m.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(m, "");
    }
}

// ---------------------------------------------------------------- drawing

fn st(c: ratatui::style::Color) -> Style {
    Style::new().fg(c)
}

fn put(buf: &mut Buffer, x: u16, y: u16, line: Line, area: Rect) {
    if y >= area.top() && y < area.bottom() && x < area.right() {
        buf.set_line(x, y, &line, area.right() - x);
    }
}

/// The wordmark, lowercase, drawn with block elements (4 rows, 38 cols).
const WORDMARK: [&str; 4] = [
    "    ██                                ",
    " ▄▄▄██ ██▄▀▀ ▄▀▀▀▄ ██   ██ ▄▀▀▀▄ ██▄▀▀",
    "██  ██ ██    ▄▄▄██ ██ █ ██ ██▀▀▀ ██   ",
    "▀█▄▄██ ██    ▀▄▄██ ▀█▀ ▀█▀ ▀█▄▄▄ ██   ",
];

/// A filing cabinet with the middle drawer pulled out past the frame, its
/// index cards showing your machines. 26 cols × 13 rows.
fn cabinet(buf: &mut Buffer, x: u16, y: u16, area: Rect, machines: &[&str]) {
    let rows: [&str; 13] = [
        "  ┌────────────────────┐  ",
        "  │ ┌────────────────┐ │  ",
        "  │ │       ━━       │ │  ",
        "  │ └────────────────┘ │  ",
        "┌─┴────────────────────┴─┐",
        "│                        │",
        "│                        │",
        "│           ━━           │",
        "└─┬────────────────────┬─┘",
        "  │ ┌────────────────┐ │  ",
        "  │ │       ━━       │ │  ",
        "  │ └────────────────┘ │  ",
        "  └────────────────────┘  ",
    ];
    for (i, r) in rows.iter().enumerate() {
        let style = if (4..=8).contains(&i) { st(BRASS) } else { st(RULE) };
        put(buf, x, y + i as u16, Line::styled(*r, style), area);
    }
    // Index cards standing in the open drawer, one tab per machine.
    let names: Vec<&str> =
        if machines.is_empty() { vec!["your-server"] } else { machines.iter().take(2).copied().collect() };
    for (i, name) in names.iter().enumerate() {
        let n: String = name.chars().take(18).collect();
        put(
            buf,
            x + 2,
            y + 5 + i as u16,
            Line::from(vec![
                Span::styled("▌", st(tab_for(name))),
                Span::raw(" "),
                Span::styled(n, st(IVORY).add_modifier(Modifier::BOLD)),
            ]),
            area,
        );
    }
}

/// Keep the end of a path that is too long, e.g. `…/config.d/lab.conf`.
fn clip_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

fn dots(step: usize) -> Line<'static> {
    let mut v = Vec::new();
    for i in 0..STEPS {
        if i > 0 {
            v.push(Span::raw(" "));
        }
        v.push(if i == step { Span::styled("●", st(BRASS)) } else { Span::styled("○", st(FAINT)) });
    }
    Line::from(v)
}

fn key(k: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(k.to_string(), st(BRASS).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {label}"), st(GRAIN)),
    ]
}

fn check(ok: bool, text: Vec<Span<'static>>) -> Line<'static> {
    let mut v = vec![if ok {
        Span::styled("✓ ", st(OK).add_modifier(Modifier::BOLD))
    } else {
        Span::styled("! ", st(BRASS).add_modifier(Modifier::BOLD))
    }];
    v.extend(text);
    Line::from(v)
}

pub fn draw(f: &mut Frame, ob: &Onboarding, hosts: &[Host]) {
    let area = f.area();
    f.render_widget(Clear, area);
    f.render_widget(Block::new().style(Style::new().bg(OAK).fg(IVORY)), area);
    let w = 76u16.min(area.width.saturating_sub(2));
    let h = 21u16.min(area.height.saturating_sub(1));
    let card = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let buf = f.buffer_mut();
    Block::bordered().border_style(st(RULE)).render(card, buf);
    for x in card.left()..card.right() {
        let sym = if x == card.left() {
            "┍"
        } else if x == card.right() - 1 {
            "┑"
        } else {
            "━"
        };
        buf[(x, card.y)].set_symbol(sym).set_fg(RED_RULE);
    }
    let titles = ["welcome", "your setup", "how it works"];
    put(
        buf,
        card.x + 2,
        card.y,
        Line::styled(format!(" {} ", tracked(titles[ob.step])), st(IVORY).add_modifier(Modifier::BOLD)),
        card,
    );
    let inner = Rect::new(card.x + 3, card.y + 2, card.width.saturating_sub(6), card.height.saturating_sub(4));

    match ob.step {
        0 => welcome(buf, inner, hosts),
        1 => setup(buf, inner, &ob.setup, hosts),
        _ => how(buf, inner),
    }

    // Footer: progress dots left, keys right.
    let fy = card.bottom() - 2;
    put(buf, inner.x, fy, dots(ob.step), card);
    let mut keys = Vec::new();
    if ob.step > 0 {
        keys.extend(key("←", "back"));
        keys.push(Span::raw("   "));
    }
    if ob.step + 1 < STEPS {
        keys.extend(key("→", "next"));
        keys.push(Span::raw("   "));
        keys.extend(key("esc", "skip"));
    } else {
        keys.extend(key("enter", "start"));
    }
    let line = Line::from(keys);
    let lw = line.width() as u16;
    put(buf, inner.right().saturating_sub(lw), fy, line, card);
}

fn welcome(buf: &mut Buffer, r: Rect, hosts: &[Host]) {
    let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    let wide = r.width >= 64;
    let tx = if wide { r.x + 29 } else { r.x };
    if wide {
        cabinet(buf, r.x, r.y + 1, r, &names);
    }
    let ty = if wide { r.y + 3 } else { r.y + 1 };
    for (i, row) in WORDMARK.iter().enumerate() {
        put(buf, tx, ty + i as u16, Line::styled(*row, st(IVORY).add_modifier(Modifier::BOLD)), r);
    }
    let lines = [
        Line::styled("Your machines, one keystroke away.", st(IVORY).add_modifier(Modifier::BOLD)),
        Line::raw(""),
        Line::styled("Pick a machine, work in its shell,", st(GRAIN)),
        Line::styled("and pull files down with one key.", st(GRAIN)),
    ];
    for (i, l) in lines.into_iter().enumerate() {
        put(buf, tx, ty + 6 + i as u16, l, r);
    }
}

fn setup(buf: &mut Buffer, r: Rect, s: &Setup, hosts: &[Host]) {
    let mut y = r.y + 1;
    let ssh = match &s.ssh_version {
        Some(v) => check(
            true,
            vec![
                Span::styled("OpenSSH ", st(IVORY)),
                Span::styled(v.trim_start_matches("OpenSSH_").to_string(), st(GRAIN)),
            ],
        ),
        None => check(false, vec![Span::styled("OpenSSH client not found. Install openssh first.", st(ERR))]),
    };
    put(buf, r.x, y, ssh, r);
    y += 2;

    let n = hosts.len();
    if n == 0 {
        put(buf, r.x, y, check(false, vec![Span::styled("No machines in your ssh config yet.", st(IVORY))]), r);
        put(
            buf,
            r.x + 2,
            y + 1,
            Line::from(
                [vec![Span::styled("Press ", st(GRAIN))], key("a", "in the machines pane to add one.")].concat(),
            ),
            r,
        );
        y += 3;
    } else {
        let file = hosts
            .iter()
            .find_map(|h| h.source.clone())
            .map(|p| download::tilde(&p))
            .unwrap_or_else(|| "~/.ssh/config".into());
        put(
            buf,
            r.x,
            y,
            check(
                true,
                vec![
                    Span::styled(
                        format!("{n} machine{} ", if n == 1 { "" } else { "s" }),
                        st(IVORY).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("from {}", clip_left(&file, (r.width as usize).saturating_sub(20))),
                        st(GRAIN),
                    ),
                ],
            ),
            r,
        );
        y += 1;
        let mut x = r.x + 2;
        for h in hosts.iter().take(6) {
            let label = format!("{}  ", h.alias);
            if x + label.len() as u16 + 2 > r.right() {
                break;
            }
            put(
                buf,
                x,
                y,
                Line::from(vec![
                    Span::styled("▌", st(tab_for(&h.alias))),
                    Span::styled(format!(" {label}"), st(IVORY)),
                ]),
                r,
            );
            x += label.chars().count() as u16 + 2;
        }
        y += 2;
    }

    match &s.pubkey {
        Some(p) => {
            put(
                buf,
                r.x,
                y,
                check(true, vec![Span::styled("SSH key ", st(IVORY)), Span::styled(download::tilde(p), st(GRAIN))]),
                r,
            );
            put(
                buf,
                r.x + 2,
                y + 1,
                Line::from(
                    [
                        vec![Span::styled("Connect to a machine and press ", st(GRAIN))],
                        key("K", "to stop typing its password."),
                    ]
                    .concat(),
                ),
                r,
            );
        }
        None => {
            put(
                buf,
                r.x,
                y,
                check(false, vec![Span::styled("No SSH key yet. Passwords work, but a key is safer:", st(IVORY))]),
                r,
            );
            put(buf, r.x + 2, y + 1, Line::styled("ssh-keygen -t ed25519", st(BRASS)), r);
        }
    }
    y += 3;
    put(
        buf,
        r.x,
        y,
        check(
            true,
            vec![Span::styled("Downloads go to ", st(IVORY)), Span::styled(download::tilde(&s.downloads), st(GRAIN))],
        ),
        r,
    );
    y += 2;
    put(
        buf,
        r.x,
        y,
        Line::styled(
            "Host keys are checked by OpenSSH. Passwords never touch drawer.",
            st(FAINT).add_modifier(Modifier::ITALIC),
        ),
        r,
    );
}

fn how(buf: &mut Buffer, r: Rect) {
    // A miniature of the real screen, each pane with its keys.
    let cols: [(&str, &[(&str, &str)]); 3] = [
        ("machines", &[("enter", "connect"), ("a e D", "add/edit/del"), ("/", "search"), ("K", "copy key")]),
        ("shell", &[("", "type as usual"), ("", "shift+pgup scrolls"), ("", ""), ("", "")]),
        ("files", &[("d", "download"), ("u", "upload"), ("h", "go up"), (".", "hidden")]),
    ];
    let gap = 1u16;
    let pw = (r.width.saturating_sub(gap * 2)) / 3;
    let ph = 7u16;
    let y = r.y + 1;
    for (i, (title, keys)) in cols.iter().enumerate() {
        let x = r.x + i as u16 * (pw + gap);
        let pane = Rect::new(x, y, pw, ph);
        Block::bordered().border_style(st(RULE)).render(pane, buf);
        put(buf, x + 2, y, Line::styled(format!(" {} ", tracked(title)), st(GRAIN)), pane);
        put(buf, x + 1, y + ph, Line::styled(format!("alt+{}", i + 1), st(BRASS).add_modifier(Modifier::BOLD)), r);
        for (j, (k, l)) in keys.iter().enumerate() {
            if k.is_empty() && l.is_empty() {
                continue;
            }
            let line = if k.is_empty() {
                Line::styled(l.to_string(), st(FAINT).add_modifier(Modifier::ITALIC))
            } else {
                Line::from(vec![
                    Span::styled(format!("{k:<5}"), st(BRASS).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {l}"), st(GRAIN)),
                ])
            };
            put(buf, x + 2, y + 1 + j as u16 + 1, line, Rect::new(x + 1, y, pw.saturating_sub(2), ph));
        }
    }
    let ty = y + ph + 2;
    put(
        buf,
        r.x,
        ty,
        Line::from(
            [
                key("alt+1 2 3", "switch panes"),
                vec![Span::raw("   ")],
                key("?", "every key"),
                vec![Span::raw("   ")],
                key("alt+q", "quit"),
            ]
            .concat(),
        ),
        r,
    );
    put(
        buf,
        r.x,
        ty + 2,
        Line::styled(
            "In the shell every key goes to the machine; use alt+1 to get back.",
            st(FAINT).add_modifier(Modifier::ITALIC),
        ),
        r,
    );
}
