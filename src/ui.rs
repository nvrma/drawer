//! Rendering. Oak cabinet, catalog-card panes, brass only on what you can act on.

use chrono::{Local, NaiveDate, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};
use tui_term::widget::{Cursor, PseudoTerminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{
    App, FORM_HINTS, FORM_LABELS, Focus, Forwards, Hit, NameInput, NameOp, PREVIEW_LIMIT, Panes, Popup, Preview,
    PreviewBody, Tone, Transfer,
};
use crate::download;
use crate::remote::{self, Forward, Kind};
use crate::session::{Sort, State};
use crate::theme::*;

// ------------------------------------------------------------------ layout

pub fn layout(area: Rect, focus: Focus, zoom: bool) -> Panes {
    let mut p = Panes::default();
    if area.width < 60 || area.height < 14 {
        p.too_small = true;
        return p;
    }
    let body_h = area.height - 1;
    p.footer = Rect::new(area.x, area.y + body_h, area.width, 1);
    if zoom {
        // alt+z: the shell takes the whole window.
        p.machines = Rect::new(area.x, area.y, 0, body_h);
        p.shell = Some(Rect::new(area.x, area.y, area.width, body_h));
        return p;
    }
    let w = area.width;
    let side = if w >= 110 { 28 } else { 24 };
    p.machines = Rect::new(area.x, area.y, side, body_h);
    let rest_x = area.x + side;
    let rest_w = w - side;
    if w >= 100 {
        let files = if w >= 150 {
            46
        } else if w >= 140 {
            42
        } else if w >= 120 {
            38
        } else {
            34
        };
        p.shell = Some(Rect::new(rest_x, area.y, rest_w - files, body_h));
        p.files = Some(Rect::new(rest_x + rest_w - files, area.y, files, body_h));
    } else if focus == Focus::Files {
        // Narrow window: the files pane takes the shell's place while focused.
        p.files = Some(Rect::new(rest_x, area.y, rest_w, body_h));
    } else {
        p.shell = Some(Rect::new(rest_x, area.y, rest_w, body_h));
    }
    p
}

// ----------------------------------------------------------------- helpers

fn st(fg: Color) -> Style {
    Style::new().fg(fg)
}

/// Metadata voice: faint italic.
fn meta() -> Style {
    st(FAINT).add_modifier(Modifier::ITALIC)
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `max` columns, keeping the start.
fn clip(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncate to `max` columns, keeping the end (for paths).
fn clip_left(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut rev = Vec::new();
    let mut w = 0;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        rev.push(c);
        w += cw;
    }
    let mut out = String::from("…");
    out.extend(rev.into_iter().rev());
    out
}

/// Strip control characters from remote-controlled strings before drawing.
fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

fn compact_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", U[i])
    } else {
        format!("{v:.0}{}", U[i])
    }
}

fn short_date(ts: Option<i64>) -> String {
    let Some(dt) = ts.and_then(|t| Local.timestamp_opt(t, 0).single()) else { return String::new() };
    let age = Local::now().signed_duration_since(dt);
    if age.num_days() < 180 && age.num_days() > -1 {
        dt.format("%b %e").to_string()
    } else {
        dt.format(" %Y").to_string()
    }
}

fn stamp_text(d: NaiveDate) -> String {
    d.format("%d %b %Y").to_string().to_uppercase()
}

fn put(buf: &mut Buffer, x: u16, y: u16, line: Line, max: u16) {
    buf.set_line(x, y, &line, max);
}

fn fill(buf: &mut Buffer, area: Rect, bg: Color) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_bg(bg);
        }
    }
}

/// A catalog-card frame: ruled hairline border; the focused pane gets the
/// card's red header rule.
fn frame(buf: &mut Buffer, area: Rect, focused: bool, title: Vec<Span>, right: Vec<Span>) -> Rect {
    let block = Block::bordered().border_style(st(RULE)).style(Style::new().bg(PANEL));
    let inner = block.inner(area);
    block.render(area, buf);
    if focused {
        for x in area.left()..area.right() {
            let sym = if x == area.left() {
                "┍"
            } else if x == area.right() - 1 {
                "┑"
            } else {
                "━"
            };
            buf[(x, area.y)].set_symbol(sym).set_fg(RED_RULE);
        }
    }
    let mut t = vec![Span::raw(" ")];
    t.extend(title);
    t.push(Span::raw(" "));
    put(buf, area.x + 2, area.y, Line::from(t), area.width.saturating_sub(4));
    if !right.is_empty() {
        let mut r = vec![Span::raw(" ")];
        r.extend(right);
        r.push(Span::raw(" "));
        let line = Line::from(r);
        let w = line.width() as u16;
        if w + 6 < area.width {
            put(buf, area.right() - 2 - w, area.y, line, w);
        }
    }
    inner
}

fn title_style(focused: bool) -> Style {
    if focused { st(IVORY).add_modifier(Modifier::BOLD) } else { st(GRAIN) }
}

fn key_hint(k: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(k.to_string(), st(BRASS).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {label}"), st(GRAIN)),
    ]
}

/// The key a hint label stands for, so clicking the hint can press it.
pub fn hint_key(k: &str) -> Option<KeyEvent> {
    let (mods, rest) = if let Some(r) = k.strip_prefix("alt+") {
        (KeyModifiers::ALT, r)
    } else if let Some(r) = k.strip_prefix("ctrl+") {
        (KeyModifiers::CONTROL, r)
    } else {
        (KeyModifiers::NONE, k)
    };
    let code = match rest {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "del" => KeyCode::Delete,
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next()?),
        _ => return None,
    };
    let mods =
        if matches!(code, KeyCode::Char(c) if c.is_ascii_uppercase()) { mods | KeyModifiers::SHIFT } else { mods };
    Some(KeyEvent::new(code, mods))
}

/// A popup's action row: `key label` pairs 5 columns apart, each clickable.
fn actions(buf: &mut Buffer, app: &App, x: u16, y: u16, w: u16, items: &[(&str, &str, Color)]) {
    let mut cx = x;
    for (i, (k, l, c)) in items.iter().enumerate() {
        if i > 0 {
            cx += 5;
        }
        // A faint key can't act right now: faint italic label, no brass.
        let label = if *c == FAINT { meta() } else { st(GRAIN) };
        let key = if *c == FAINT { st(FAINT) } else { st(*c).add_modifier(Modifier::BOLD) };
        let line = Line::from(vec![Span::styled(k.to_string(), key), Span::styled(format!(" {l}"), label)]);
        let lw = line.width() as u16;
        if cx + lw > x + w {
            break;
        }
        put(buf, cx, y, line, lw);
        if let Some(ev) = hint_key(k) {
            app.hit(Rect::new(cx, y, lw, 1), Hit::Key(ev));
        }
        cx += lw;
    }
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (k, l)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.extend(key_hint(k, l));
    }
    Line::from(spans)
}

// -------------------------------------------------------------------- draw

pub fn draw(f: &mut Frame, app: &App) {
    app.hits.borrow_mut().clear();
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(OAK).fg(IVORY)), area);
    let p = app.panes;
    if p.too_small {
        let msg = Paragraph::new(vec![
            Line::styled("drawer needs a bigger window", st(IVORY).add_modifier(Modifier::BOLD)),
            Line::styled("at least 60 × 14", st(GRAIN)),
        ])
        .alignment(Alignment::Center);
        let y = area.height / 2;
        f.render_widget(msg, Rect::new(area.x, area.y + y.saturating_sub(1), area.width, 2));
        return;
    }
    if p.machines.width > 0 {
        draw_machines(f.buffer_mut(), p.machines, app);
    }
    if let Some(r) = p.shell {
        draw_shell(f, r, app);
    }
    if let Some(r) = p.files {
        draw_files(f.buffer_mut(), r, app);
    }
    draw_footer(f.buffer_mut(), p.footer, app);
    if let Some(popup) = &app.popup {
        draw_popup(f, area, app, popup);
    }
    if let Some(ob) = &app.onboarding {
        crate::onboarding::draw(f, ob, &app.hosts);
    }
}

fn draw_machines(buf: &mut Buffer, area: Rect, app: &App) {
    let focused = app.focus == Focus::Machines;
    let inner = frame(buf, area, focused, vec![Span::styled(tracked("machines"), title_style(focused))], vec![]);
    let w = inner.width as usize;

    if !app.hosts_loaded {
        put(buf, inner.x + 1, inner.y + 1, Line::styled("Reading ~/.ssh/config…", st(GRAIN)), inner.width - 1);
        return;
    }
    if app.hosts.is_empty() {
        let lines = vec![
            Line::styled("No machines yet", st(IVORY).add_modifier(Modifier::BOLD)),
            Line::raw(""),
            Line::from(vec![
                Span::styled("a", st(BRASS).add_modifier(Modifier::BOLD)),
                Span::styled(" add a machine", st(GRAIN)),
            ]),
            Line::raw(""),
            Line::styled("drawer lists the Host", st(GRAIN)),
            Line::styled("entries in your ssh", st(GRAIN)),
            Line::styled("config and saves new", st(GRAIN)),
            Line::styled("ones there too.", st(GRAIN)),
        ];
        Paragraph::new(lines).wrap(Wrap { trim: false }).render(
            Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), inner.height.saturating_sub(1)),
            buf,
        );
        return;
    }

    // Search line.
    let mut top = inner.y;
    if app.searching || !app.filter.is_empty() {
        let mut spans = vec![
            Span::styled("/ ", st(BRASS).add_modifier(Modifier::BOLD)),
            Span::styled(clip(&app.filter, w.saturating_sub(4)), st(IVORY)),
        ];
        if app.searching {
            spans.push(Span::styled("▏", st(BRASS)));
        } else {
            spans.push(Span::styled("  esc clears", meta()));
        }
        put(buf, inner.x + 1, top, Line::from(spans), inner.width - 1);
        top += 2;
    }
    let vis = app.visible_hosts();
    if vis.is_empty() {
        put(buf, inner.x + 1, top, Line::styled("No match", st(GRAIN)), inner.width - 1);
    }

    // Two lines per drawer, one line of air between.
    let list_h = (inner.bottom().saturating_sub(top)).saturating_sub(2) as usize;
    let per = 3;
    let fits = list_h.div_ceil(per).max(1);
    let sel_pos = vis.iter().position(|&i| i == app.selected).unwrap_or(0);
    let offset = sel_pos.saturating_sub(fits - 1);
    for (row, &i) in vis.iter().skip(offset).take(fits).enumerate() {
        let h = &app.hosts[i];
        let y = top + (row * per) as u16;
        let hue = tab_for(&h.alias);
        let sel = i == app.selected;
        if sel {
            fill(buf, Rect::new(inner.x, y, inner.width, 2), ROW_SEL);
        }
        app.hit(Rect::new(inner.x, y, inner.width, 2.min(inner.bottom().saturating_sub(y))), Hit::Machine(i));
        let session = app.sessions.get(&h.alias);
        let (glyph, gstyle) = match session.map(|s| &s.state) {
            Some(State::Connected) => ("●", st(OK)),
            Some(State::Connecting) => ("◌", st(IVORY)),
            Some(State::Failed(_)) => ("✕", st(ERR)),
            Some(State::Ended(_)) => ("○", st(FAINT)),
            None => (" ", st(FAINT)),
        };
        let name_style = if sel && focused {
            st(BRASS).add_modifier(Modifier::BOLD)
        } else {
            st(IVORY).add_modifier(Modifier::BOLD)
        };
        let name = clip(&h.alias, w.saturating_sub(5));
        put(
            buf,
            inner.x,
            y,
            Line::from(vec![Span::styled("▌", st(hue)), Span::raw(" "), Span::styled(name, name_style)]),
            inner.width,
        );
        put(buf, inner.right() - 2, y, Line::styled(glyph, gstyle), 1);

        let detail = if app.attention.contains(&h.alias) {
            let text = match session.map(|s| &s.state) {
                Some(State::Ended(_)) => "connection dropped",
                _ => "couldn't connect",
            };
            Span::styled(text, st(ERR))
        } else {
            let style = if sel { st(GRAIN).add_modifier(Modifier::ITALIC) } else { meta() };
            Span::styled(clip(&h.address(), w.saturating_sub(3)), style)
        };
        put(buf, inner.x, y + 1, Line::from(vec![Span::styled("▌", st(hue)), Span::raw(" "), detail]), inner.width);
    }

    let live = app.live_sessions();
    let n = app.hosts.len();
    let mut summary = if vis.len() == n {
        format!("{n} machine{}", if n == 1 { "" } else { "s" })
    } else {
        format!("{} of {n}", vis.len())
    };
    if live > 0 {
        summary.push_str(&format!(" · {live} open"));
    }
    put(buf, inner.x + 1, inner.bottom() - 1, Line::styled(summary, st(FAINT)), inner.width - 1);
}

fn draw_shell(f: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Shell;
    let host = app.current_host();
    let session = app.current_session();
    let mut title = Vec::new();
    match host {
        Some(h) => {
            title.push(Span::styled(
                format!(" {} ", h.alias),
                Style::new().bg(tab_for(&h.alias)).fg(ON_TAB).add_modifier(Modifier::BOLD),
            ));
            let state = match session.map(|s| &s.state) {
                Some(State::Connecting) => Some(("connecting", st(GRAIN))),
                Some(State::Connected) => None,
                Some(State::Ended(_)) => Some(("ended", st(FAINT))),
                Some(State::Failed(_)) => Some(("couldn't connect", st(ERR))),
                None => Some(("not connected", st(FAINT))),
            };
            if let Some((t, s)) = state {
                title.push(Span::styled(format!("  {t}"), s));
            }
        }
        None => title.push(Span::styled(tracked("shell"), title_style(focused))),
    }
    let mut right = Vec::new();
    if let Some(s) = session {
        if !s.forwards.is_empty() {
            let ports: Vec<String> = s.forwards.iter().map(|f| format!(":{}", f.port())).collect();
            right.push(Span::styled(format!("↔ {}", ports.join(" ")), st(GRAIN)));
        }
        let back = s.scrollback();
        if back > 0 {
            if !right.is_empty() {
                right.push(Span::styled("  ·  ", st(FAINT)));
            }
            right.push(Span::styled(format!("↑ {back} line{}", if back == 1 { "" } else { "s" }), st(GRAIN)));
        }
    }
    let inner = frame(f.buffer_mut(), area, focused, title, right);

    let Some(h) = host else {
        let lines = vec![
            Line::styled("No machine selected", st(IVORY).add_modifier(Modifier::BOLD)),
            Line::styled("Add machines to ~/.ssh/config to begin.", st(GRAIN)),
        ];
        let y = inner.y + inner.height / 2 - 1;
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), Rect::new(inner.x, y, inner.width, 2));
        return;
    };

    let Some(s) = session else {
        let lines = vec![
            Line::styled(h.alias.clone(), st(tab_for(&h.alias)).add_modifier(Modifier::BOLD)),
            Line::styled(h.address(), st(GRAIN)),
            Line::raw(""),
            hints(&[("enter", "connect")]),
        ];
        let y = inner.y + (inner.height / 2).saturating_sub(2);
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), Rect::new(inner.x, y, inner.width, 4));
        return;
    };

    if let Ok(parser) = s.parser.lock() {
        let screen = parser.screen();
        let mut cursor = Cursor::default();
        cursor.hide();
        f.render_widget(PseudoTerminal::new(screen).cursor(cursor), inner);
        // Terminal default colours become the pane's.
        let buf = f.buffer_mut();
        for y in inner.top()..inner.bottom() {
            for x in inner.left()..inner.right() {
                let c = &mut buf[(x, y)];
                if c.bg == Color::Reset {
                    c.set_bg(PANEL);
                }
                if c.fg == Color::Reset {
                    c.set_fg(IVORY);
                }
            }
        }
        // Mouse selection: the theme's selection grey under default text.
        if let Some(sel) = app.selection.as_ref().filter(|sel| sel.alias == s.alias) {
            for row in 0..inner.height {
                for col in 0..inner.width {
                    if sel.contains(row, col) {
                        let c = &mut buf[(inner.x + col, inner.y + row)];
                        c.set_bg(ROW_SEL).set_fg(IVORY);
                        c.modifier.remove(Modifier::REVERSED | Modifier::DIM);
                    }
                }
            }
        }
        if focused && s.is_live() && screen.scrollback() == 0 && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            if row < inner.height && col < inner.width {
                f.set_cursor_position((inner.x + col, inner.y + row));
            }
        }
    }

    if !s.is_live() {
        let failed = matches!(s.state, State::Failed(_));
        let msg = Line::styled(
            if failed { " Couldn't connect. " } else { " Session ended. " },
            st(if failed { ERR } else { IVORY }).add_modifier(Modifier::BOLD),
        );
        let bar = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let buf = f.buffer_mut();
        fill(buf, bar, CARD);
        for x in bar.left()..bar.right() {
            buf[(x, bar.y)].set_symbol(" ");
        }
        let mw = msg.width() as u16;
        put(buf, bar.x, bar.y, msg, bar.width);
        let again = if failed { "try again" } else { "reconnect" };
        actions(
            buf,
            app,
            bar.x + mw + 2,
            bar.y,
            bar.width.saturating_sub(mw + 2),
            &[("enter", again, BRASS), ("x", "close", BRASS)],
        );
    }
}

fn draw_files(buf: &mut Buffer, area: Rect, app: &App) {
    let focused = app.focus == Focus::Files;
    let host = app.current_host();
    let session = app.current_session();
    let hue = host.map(|h| tab_for(&h.alias)).unwrap_or(GRAIN);
    let inner = frame(buf, area, focused, vec![Span::styled(tracked("files"), title_style(focused))], vec![]);
    let w = inner.width as usize;
    let note = |buf: &mut Buffer, lines: Vec<Line>| {
        Paragraph::new(lines).wrap(Wrap { trim: true }).render(
            Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), inner.height.saturating_sub(1)),
            buf,
        );
    };

    let Some(s) = session else {
        note(buf, vec![Line::styled("Connect to a machine to see its files.", st(GRAIN))]);
        return;
    };
    let files = &s.files;
    match s.state {
        State::Connecting => return note(buf, vec![Line::styled("Waiting for login…", st(GRAIN))]),
        State::Ended(_) | State::Failed(_) if files.sftp.is_none() => {
            return note(buf, vec![Line::styled("Not connected.", st(GRAIN))]);
        }
        _ => {}
    }
    if files.sftp.is_none() {
        if let Some(e) = &files.error {
            return note(
                buf,
                vec![
                    Line::styled("File browser unavailable", st(ERR).add_modifier(Modifier::BOLD)),
                    Line::styled(safe(e), st(GRAIN)),
                    Line::raw(""),
                    hints(&[("r", "try again")]),
                ],
            );
        }
        return note(buf, vec![Line::styled("Opening files…", st(GRAIN))]);
    }

    // Path line in the machine's hue, then the card's printed rule.
    let display = if !files.home.is_empty() && files.cwd.starts_with(&files.home) {
        format!("~{}", &files.cwd[files.home.len()..])
    } else {
        files.cwd.clone()
    };
    put(
        buf,
        inner.x,
        inner.y,
        Line::from(vec![
            Span::styled("▌", st(hue)),
            Span::styled(format!(" {}", clip_left(&safe(&display), w.saturating_sub(3))), st(IVORY)),
        ]),
        inner.width,
    );
    for x in inner.left()..inner.right() {
        buf[(x, inner.y + 1)].set_symbol("─").set_fg(RULE);
    }

    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(3));
    let visible = files.visible();
    if let Some(e) = &files.error {
        Paragraph::new(vec![Line::styled(safe(e), st(ERR)), Line::raw(""), hints(&[("h", "back"), ("r", "retry")])])
            .wrap(Wrap { trim: true })
            .render(Rect::new(list.x + 1, list.y, list.width.saturating_sub(2), list.height), buf);
    } else if files.loading && files.entries.is_empty() {
        put(buf, list.x + 1, list.y, Line::styled("Reading…", st(GRAIN)), list.width - 1);
    } else if visible.is_empty() && !files.filter.is_empty() {
        put(buf, list.x + 1, list.y, Line::styled("Nothing matches.", st(GRAIN)), list.width - 1);
    } else if visible.is_empty() {
        put(buf, list.x + 1, list.y, Line::styled("This folder is empty.", st(GRAIN)), list.width - 1);
    } else {
        let icons = icons();
        let rows = list.height as usize;
        let offset = files.selected.saturating_sub(rows.saturating_sub(1));
        let alias = host.map(|h| h.alias.as_str()).unwrap_or("");
        // Date-due stamp column, reserved only when this folder holds a
        // downloaded file so other folders keep their full name width.
        let stamped = |name: &str| app.stamps.get(&(alias.to_string(), crate::remote::join(&files.cwd, name))).copied();
        let any_stamp = visible.iter().any(|e| stamped(&e.name).is_some());
        let mark_w: usize = if any_stamp { 7 } else { 0 };
        const SIZE_W: usize = 5;
        const DATE_W: usize = 6;
        let name_w = w.saturating_sub(2 + 1 + mark_w + 1 + SIZE_W + 1 + DATE_W + 1);
        for (row, (i, e)) in visible.iter().enumerate().skip(offset).take(rows).enumerate() {
            let y = list.y + row as u16;
            let sel = i == files.selected;
            if sel {
                fill(buf, Rect::new(list.x, y, list.width, 1), ROW_SEL);
            }
            app.hit(Rect::new(list.x, y, list.width, 1), Hit::File(i));
            let (icon, mut name_style) = match e.kind {
                Kind::Dir => (icons.folder, st(IVORY).add_modifier(Modifier::BOLD)),
                Kind::LinkDir => (icons.link, st(IVORY).add_modifier(Modifier::BOLD | Modifier::ITALIC)),
                Kind::LinkFile => (icons.link, st(IVORY).add_modifier(Modifier::ITALIC)),
                Kind::File => (icons.file, st(IVORY)),
                Kind::Other => (icons.file, st(FAINT)),
            };
            if sel && focused {
                name_style = name_style.fg(BRASS);
            }
            let name = if e.kind.is_dir() {
                clip(&format!("{}/", safe(&e.name)), name_w)
            } else {
                clip(&safe(&e.name), name_w)
            };
            let pad = name_w.saturating_sub(width(&name));
            let size = if e.kind.is_dir() { String::new() } else { compact_size(e.size) };
            let mark = match stamped(&e.name) {
                Some(d) if mark_w > 0 => Span::styled(
                    format!("{:>mark_w$}", format!("✓{}", d.format("%d %b").to_string().to_uppercase())),
                    st(STAMP).add_modifier(Modifier::BOLD),
                ),
                _ => Span::raw(" ".repeat(mark_w)),
            };
            let date_style = if sel { st(GRAIN).add_modifier(Modifier::ITALIC) } else { meta() };
            let date = Span::styled(format!("{:>DATE_W$}", short_date(e.mtime)), date_style);
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(icon, st(GRAIN)),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" ".repeat(pad)),
                Span::raw(" "),
                mark,
                Span::styled(format!(" {size:>SIZE_W$}"), st(GRAIN)),
                Span::raw(" "),
                date,
            ]);
            put(buf, list.x, y, line, list.width);
        }
    }

    // Status row; while filtering it becomes the filter's input line.
    let status = if files.filtering || !files.filter.is_empty() {
        let mut spans = vec![
            Span::styled("/ ", st(BRASS).add_modifier(Modifier::BOLD)),
            Span::styled(clip_left(&files.filter, w.saturating_sub(16)), st(IVORY)),
        ];
        if files.filtering {
            spans.push(Span::styled("▏", st(BRASS)));
        }
        spans.push(Span::styled(format!("  {} found", visible.len()), meta()));
        spans
    } else {
        let hidden = files.hidden_count();
        let mut spans = vec![Span::styled(
            format!("{} item{}", visible.len(), if visible.len() == 1 { "" } else { "s" }),
            st(FAINT),
        )];
        if hidden > 0 {
            spans.push(Span::styled(format!(" · {hidden} hidden "), st(FAINT)));
            spans.push(Span::styled(".", st(BRASS).add_modifier(Modifier::BOLD)));
        }
        if files.sort != Sort::Name {
            spans.push(Span::styled(format!(" · {} first", files.sort.label()), st(FAINT)));
        }
        if files.loading && !files.entries.is_empty() {
            spans.push(Span::styled(" · reading…", st(FAINT)));
        }
        spans
    };
    put(buf, inner.x + 1, inner.bottom() - 1, Line::from(status), inner.width - 1);
}

fn draw_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let session = app.current_session();
    let live = session.is_some_and(|s| s.is_live());
    let scrolled = session.is_some_and(|s| s.scrollback() > 0);
    let filtering = session.is_some_and(|s| s.files.filtering);
    let connected = session.is_some_and(|s| s.state == State::Connected);
    let pairs: &[(&str, &str)] = match app.focus {
        Focus::Machines if app.hosts.is_empty() => &[("a", "add"), ("r", "reload"), ("?", "help"), ("q", "quit")],
        Focus::Machines if app.searching => &[("enter", "connect"), ("esc", "clear"), ("↑↓", "move")],
        Focus::Machines if connected => &[
            ("enter", "shell"),
            ("f", "forward port"),
            ("K", "copy key"),
            ("x", "disconnect"),
            ("a", "add"),
            ("e", "edit"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Focus::Machines => &[
            ("enter", "connect"),
            ("a", "add"),
            ("e", "edit"),
            ("D", "delete"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Focus::Shell if scrolled => {
            &[("esc", "back to live"), ("↑↓", "line"), ("pgup", "page up"), ("pgdn", "page down"), ("wheel", "scroll")]
        }
        Focus::Shell if live && app.zoom => {
            &[("alt+z", "unzoom"), ("alt+pgup", "scroll back"), ("alt+1", "machines"), ("alt+q", "quit")]
        }
        Focus::Shell if live => &[
            ("alt+1", "machines"),
            ("alt+3", "files"),
            ("alt+z", "zoom"),
            ("alt+pgup", "scroll back"),
            ("alt+q", "quit"),
        ],
        Focus::Shell => &[("enter", "connect"), ("tab", "next pane"), ("?", "help"), ("q", "quit")],
        Focus::Files if filtering => &[("enter", "keep filter"), ("esc", "clear"), ("↑↓", "move")],
        Focus::Files => &[
            ("enter", "open"),
            ("h", "back"),
            ("d", "download"),
            ("u", "upload"),
            ("/", "filter"),
            ("n", "new folder"),
            ("s", "sort"),
            ("?", "help"),
        ],
    };
    // `d` only gets brass when the selected entry can actually be downloaded.
    let can_download = app
        .current_session()
        .and_then(|s| s.files.sftp.as_ref().and(s.files.current()))
        .is_some_and(|e| matches!(e.kind, Kind::File | Kind::LinkFile | Kind::Dir));
    let groups: Vec<Vec<Span>> = pairs
        .iter()
        .map(|(k, l)| {
            if *k == "d" && !can_download {
                vec![Span::styled("d", st(FAINT)), Span::styled(format!(" {l}"), meta())]
            } else {
                key_hint(k, l)
            }
        })
        .collect();

    let right: Option<Line> = if let Some(t) = &app.toast {
        let c = match t.tone {
            Tone::Good => OK,
            Tone::Bad => ERR,
            Tone::Info => IVORY,
        };
        Some(Line::styled(format!("{} ", clip(&safe(&t.text), (area.width as usize).saturating_sub(2))), st(c)))
    } else if let Some(t) = app.transfers.first() {
        let more = if app.transfers.len() > 1 { format!(" +{}", app.transfers.len() - 1) } else { String::new() };
        Some(Line::styled(format!("↓ {} {}%{more} ", clip(&t.name, 24), pct(t)), st(STAMP)))
    } else {
        None
    };
    let right_w = right.as_ref().map(|l| l.width() + 3).unwrap_or(0);

    // Keep whole hints only, dropping from the end until the legend and the
    // status fit side by side.
    let gap = 3;
    let mut left = vec![Span::raw(" ")];
    let mut used = 1;
    for (i, g) in groups.into_iter().enumerate() {
        let w: usize = g.iter().map(|s| s.width()).sum();
        let gw = w + if i > 0 { gap } else { 0 };
        if used + gw + right_w > area.width as usize {
            break;
        }
        if i > 0 {
            left.push(Span::raw(" ".repeat(gap)));
        }
        // Clicking a hint presses its key.
        if let Some(ev) = hint_key(pairs[i].0) {
            app.hit(Rect::new(area.x + (used + gw - w) as u16, area.y, w as u16, 1), Hit::Key(ev));
        }
        left.extend(g);
        used += gw;
    }
    put(buf, area.x, area.y, Line::from(left), area.width);
    if let Some(line) = right {
        let w = (line.width() as u16).min(area.width);
        put(buf, area.right() - w, area.y, line, w);
    }
}

fn pct(t: &Transfer) -> u64 {
    (t.done * 100).checked_div(t.total).unwrap_or(0).min(100)
}

// ------------------------------------------------------------------ popups

fn card(f: &mut Frame, app: &App, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    app.hit(r, Hit::Card);
    f.render_widget(Clear, r);
    let buf = f.buffer_mut();
    let block = Block::bordered().border_style(st(RULE)).style(Style::new().bg(CARD).fg(IVORY));
    let inner = block.inner(r);
    block.render(r, buf);
    for x in r.left()..r.right() {
        let sym = if x == r.left() {
            "┍"
        } else if x == r.right() - 1 {
            "┑"
        } else {
            "━"
        };
        buf[(x, r.y)].set_symbol(sym).set_fg(RED_RULE);
    }
    put(
        buf,
        r.x + 2,
        r.y,
        Line::styled(format!(" {} ", tracked(title)), st(IVORY).add_modifier(Modifier::BOLD)),
        r.width - 4,
    );
    Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(4), inner.height.saturating_sub(2))
}

/// The DATE DUE field of the checkout slip, right-aligned to `right`.
/// Empty: a dashed ruled box waiting for ink. Stamped: a double-ruled violet
/// impression with the date. Returns the field's width.
fn due_field(buf: &mut Buffer, right: u16, y: u16, date: Option<NaiveDate>) -> u16 {
    let inside = 13u16; // " 10 NOV 2026 "
    let w = inside + 2;
    let x = right - w;
    put(buf, x + 1, y, Line::styled("DATE DUE", st(GRAIN).add_modifier(Modifier::BOLD)), w);
    let n = inside as usize;
    match date {
        None => {
            let s = st(RULE);
            put(buf, x, y + 1, Line::styled(format!("┌{}┐", "┄".repeat(n)), s), w);
            put(
                buf,
                x,
                y + 2,
                Line::from(vec![Span::styled("┆", s), Span::raw(" ".repeat(n)), Span::styled("┆", s)]),
                w,
            );
            put(buf, x, y + 3, Line::styled(format!("└{}┘", "┄".repeat(n)), s), w);
        }
        Some(d) => {
            let s = st(STAMP).add_modifier(Modifier::BOLD);
            let text = format!("{:^n$}", stamp_text(d));
            put(buf, x, y + 1, Line::styled(format!("╔{}╗", "═".repeat(n)), s), w);
            put(buf, x, y + 2, Line::from(vec![Span::styled("║", s), Span::styled(text, s), Span::styled("║", s)]), w);
            put(buf, x, y + 3, Line::styled(format!("╚{}╝", "═".repeat(n)), s), w);
        }
    }
    w
}

fn draw_popup(f: &mut Frame, area: Rect, app: &App, popup: &Popup) {
    match popup {
        Popup::Download(ask) => {
            // The checkout slip: details on the left, an empty DATE DUE field
            // on the right that pressing y stamps.
            let inner = card(f, app, area, 60, 13, if ask.is_dir { "download folder" } else { "download" });
            let buf = f.buffer_mut();
            let fw = due_field(buf, inner.right(), inner.y, None);
            let text_w = inner.width.saturating_sub(fw + 2);
            let tw = text_w as usize;
            let hue = tab_for(&ask.alias);
            put(
                buf,
                inner.x,
                inner.y,
                Line::styled(clip(&ask.name, tw), st(IVORY).add_modifier(Modifier::BOLD)),
                text_w,
            );
            put(
                buf,
                inner.x,
                inner.y + 1,
                Line::from(vec![
                    Span::styled(if ask.is_dir { "whole folder".to_string() } else { human_size(ask.size) }, st(GRAIN)),
                    Span::styled("  from  ", meta()),
                    Span::styled(ask.alias.clone(), st(hue).add_modifier(Modifier::BOLD)),
                ]),
                text_w,
            );
            put(buf, inner.x, inner.y + 2, Line::styled(clip_left(&safe(&ask.remote_path), tw), meta()), text_w);
            put(buf, inner.x, inner.y + 5, Line::styled("Saves to", st(GRAIN)), inner.width);
            put(
                buf,
                inner.x,
                inner.y + 6,
                Line::styled(clip_left(&download::tilde(&ask.target), inner.width as usize), st(IVORY)),
                inner.width,
            );
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("y", "download", BRASS), ("n", "cancel", BRASS)],
            );
        }
        Popup::Transfer(id) => {
            let Some(t) = app.transfers.iter().find(|t| t.id == *id) else { return };
            let inner = card(f, app, area, 60, 13, if t.upload { "uploading" } else { "downloading" });
            let buf = f.buffer_mut();
            // Only downloads get the date-due stamp.
            let fw = if t.upload { 0 } else { due_field(buf, inner.right(), inner.y, Some(t.stamped)) + 2 };
            let text_w = inner.width.saturating_sub(fw);
            put(
                buf,
                inner.x,
                inner.y,
                Line::styled(clip(&t.name, text_w as usize), st(IVORY).add_modifier(Modifier::BOLD)),
                text_w,
            );
            put(
                buf,
                inner.x,
                inner.y + 1,
                Line::from(vec![
                    Span::styled(
                        if t.total == 0 && t.is_dir { "counting…".to_string() } else { human_size(t.total) },
                        st(GRAIN),
                    ),
                    Span::styled(if t.upload { "  to  " } else { "  from  " }, meta()),
                    Span::styled(t.alias.clone(), st(tab_for(&t.alias)).add_modifier(Modifier::BOLD)),
                ]),
                text_w,
            );
            if t.is_dir && !t.file.is_empty() {
                put(
                    buf,
                    inner.x,
                    inner.y + 2,
                    Line::styled(clip_left(&safe(&t.file), text_w as usize), meta()),
                    text_w,
                );
            }
            // Progress: stamp ink filling a ruled line.
            let bar_w = inner.width as usize;
            let filled =
                if t.total == 0 { 0 } else { ((t.done as f64 / t.total as f64) * bar_w as f64).round() as usize }
                    .min(bar_w);
            put(
                buf,
                inner.x,
                inner.y + 5,
                Line::from(vec![
                    Span::styled("━".repeat(filled), st(STAMP)),
                    Span::styled("─".repeat(bar_w - filled), st(RULE)),
                ]),
                inner.width,
            );
            let secs = t.started.elapsed().as_secs_f64().max(0.001);
            let rate = t.done as f64 / secs;
            let mut stats =
                format!("{} of {}  ·  {}/s", human_size(t.done), human_size(t.total), human_size(rate as u64));
            if rate > 0.0 && t.total > t.done {
                let left = ((t.total - t.done) as f64 / rate) as u64;
                stats.push_str(&format!("  ·  {}:{:02} left", left / 60, left % 60));
            }
            put(buf, inner.x, inner.y + 6, Line::styled(stats, st(GRAIN)), inner.width);
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("c", "cancel", BRASS), ("esc", "hide", BRASS)],
            );
        }
        Popup::Help(scroll) => draw_help(f, area, app, *scroll),
        Popup::Preview(p) => draw_preview(f, area, app, p),
        Popup::Name(p) => draw_name(f, area, app, p),
        Popup::Forwards(p) => draw_forwards(f, area, app, p),
        Popup::ConfirmRemove { alias, dir, name, is_dir } => {
            let inner = card(f, app, area, 62, 11, if *is_dir { "delete folder" } else { "delete" });
            let buf = f.buffer_mut();
            let w = inner.width as usize;
            let shown = if *is_dir { format!("{}/", safe(name)) } else { safe(name) };
            put(
                buf,
                inner.x,
                inner.y,
                Line::from(vec![
                    Span::styled("Delete ", st(IVORY)),
                    Span::styled(
                        clip(&shown, w.saturating_sub(alias.len() + 14)),
                        st(IVORY).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" on ", st(IVORY)),
                    Span::styled(alias.clone(), st(tab_for(alias)).add_modifier(Modifier::BOLD)),
                    Span::styled("?", st(IVORY)),
                ]),
                inner.width,
            );
            put(
                buf,
                inner.x,
                inner.y + 1,
                Line::styled(clip_left(&safe(&remote::join(dir, name)), w), meta()),
                inner.width,
            );
            let warn = if *is_dir {
                "The folder and everything in it is deleted for good."
            } else {
                "It's deleted for good; there's no trash on the machine."
            };
            put(buf, inner.x, inner.y + 3, Line::styled(warn, st(GRAIN)), inner.width);
            actions(buf, app, inner.x, inner.bottom() - 1, inner.width, &[("y", "delete", ERR), ("n", "keep", BRASS)]);
        }
        Popup::HostForm(form) => {
            let title = if form.editing.is_some() { "edit machine" } else { "add machine" };
            let inner = card(f, app, area, 64, 19, title);
            let buf = f.buffer_mut();
            let label_w = 10u16;
            for (i, label) in FORM_LABELS.iter().enumerate() {
                let y = inner.y + (i as u16) * 2;
                let active = i == form.focus;
                let lstyle = if active { st(BRASS).add_modifier(Modifier::BOLD) } else { st(GRAIN) };
                put(buf, inner.x, y, Line::styled(*label, lstyle), label_w);
                let fx = inner.x + label_w;
                let fw = inner.width.saturating_sub(label_w);
                let value = &form.fields[i];
                let mut spans = if value.is_empty() && !active {
                    vec![Span::styled(FORM_HINTS[i], meta())]
                } else {
                    vec![Span::styled(clip_left(value, fw.saturating_sub(2) as usize), st(IVORY))]
                };
                if active {
                    spans.push(Span::styled("▏", st(BRASS)));
                    if value.is_empty() {
                        spans.push(Span::styled(FORM_HINTS[i], meta()));
                    }
                }
                put(buf, fx, y, Line::from(spans), fw);
                // Underline: the field's ruled line.
                for x in fx..fx + fw {
                    buf[(x, y + 1)].set_symbol("─").set_fg(if active { BRASS } else { RULE });
                }
            }
            let y = inner.y + 11;
            put(
                buf,
                inner.x,
                y,
                Line::from(vec![
                    Span::styled("Saves to  ", st(GRAIN)),
                    Span::styled(clip_left(&download::tilde(&form.file), inner.width as usize - 10), st(IVORY)),
                ]),
                inner.width,
            );
            if let Some(e) = &form.error {
                put(
                    buf,
                    inner.x,
                    y + 2,
                    Line::styled(clip(&safe(e), inner.width as usize), st(ERR).add_modifier(Modifier::BOLD)),
                    inner.width,
                );
            }
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("tab", "next field", BRASS), ("ctrl+s", "save", BRASS), ("esc", "cancel", BRASS)],
            );
        }
        Popup::ConfirmDelete { alias, file } => {
            let inner = card(f, app, area, 60, 10, "delete machine");
            let buf = f.buffer_mut();
            put(
                buf,
                inner.x,
                inner.y,
                Line::from(vec![
                    Span::styled("Remove ", st(IVORY)),
                    Span::styled(alias.clone(), st(tab_for(alias)).add_modifier(Modifier::BOLD)),
                    Span::styled(" from", st(IVORY)),
                ]),
                inner.width,
            );
            put(
                buf,
                inner.x,
                inner.y + 1,
                Line::styled(clip_left(&download::tilde(file), inner.width as usize), st(IVORY)),
                inner.width,
            );
            put(
                buf,
                inner.x,
                inner.y + 3,
                Line::styled("The old file is kept as a .drawer.bak backup.", st(GRAIN)),
                inner.width,
            );
            actions(buf, app, inner.x, inner.bottom() - 1, inner.width, &[("y", "delete", ERR), ("n", "keep", BRASS)]);
        }
        Popup::CopyKey { alias, key_path, key } => {
            let inner = card(f, app, area, 64, 11, "copy key");
            let buf = f.buffer_mut();
            put(
                buf,
                inner.x,
                inner.y,
                Line::from(vec![
                    Span::styled("Add your public key to ", st(IVORY)),
                    Span::styled(alias.clone(), st(tab_for(alias)).add_modifier(Modifier::BOLD)),
                    Span::styled("?", st(IVORY)),
                ]),
                inner.width,
            );
            put(buf, inner.x, inner.y + 1, Line::styled(download::tilde(key_path), st(IVORY)), inner.width);
            let kind = key.split_whitespace().next().unwrap_or("");
            put(buf, inner.x, inner.y + 2, Line::styled(kind.to_string(), meta()), inner.width);
            put(
                buf,
                inner.x,
                inner.y + 4,
                Line::styled("After this, logging in won't ask for a password.", st(GRAIN)),
                inner.width,
            );
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("y", "copy key", BRASS), ("n", "cancel", BRASS)],
            );
        }
        Popup::Upload(p) => {
            let inner = card(f, app, area, 68, 16, "upload");
            let buf = f.buffer_mut();
            let w = inner.width as usize;
            put(
                buf,
                inner.x,
                inner.y,
                Line::from(vec![
                    Span::styled("To  ", st(GRAIN)),
                    Span::styled(p.alias.clone(), st(tab_for(&p.alias)).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(":{}", clip_left(&safe(&p.remote_dir), w.saturating_sub(p.alias.len() + 6))),
                        st(IVORY),
                    ),
                ]),
                inner.width,
            );
            put(buf, inner.x, inner.y + 2, Line::styled("File or folder on this computer", st(GRAIN)), inner.width);
            put(
                buf,
                inner.x,
                inner.y + 3,
                Line::from(vec![
                    Span::styled(clip_left(&p.input, w.saturating_sub(2)), st(IVORY)),
                    Span::styled("▏", st(BRASS)),
                ]),
                inner.width,
            );
            for x in inner.left()..inner.right() {
                buf[(x, inner.y + 4)].set_symbol("─").set_fg(BRASS);
            }
            for (i, s) in p.suggestions.iter().take(6).enumerate() {
                put(buf, inner.x + 1, inner.y + 5 + i as u16, Line::styled(clip(s, w - 1), meta()), inner.width - 1);
            }
            if let Some(e) = &p.error {
                put(
                    buf,
                    inner.x,
                    inner.bottom() - 3,
                    Line::styled(e.clone(), st(ERR).add_modifier(Modifier::BOLD)),
                    inner.width,
                );
            }
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("tab", "complete", BRASS), ("enter", "upload", BRASS), ("esc", "cancel", BRASS)],
            );
        }
        Popup::ConfirmQuit(n) => {
            let inner = card(f, app, area, 48, 8, "quit");
            let buf = f.buffer_mut();
            let text = if *n == 1 { "1 session is open.".to_string() } else { format!("{n} sessions are open.") };
            put(buf, inner.x, inner.y, Line::styled(text, st(IVORY).add_modifier(Modifier::BOLD)), inner.width);
            put(buf, inner.x, inner.y + 1, Line::styled("Quitting closes them.", st(GRAIN)), inner.width);
            actions(buf, app, inner.x, inner.bottom() - 1, inner.width, &[("y", "quit", BRASS), ("n", "stay", BRASS)]);
        }
        Popup::ConfirmClose(alias) => {
            let inner = card(f, app, area, 48, 8, "close session");
            let buf = f.buffer_mut();
            put(
                buf,
                inner.x,
                inner.y,
                Line::from(vec![
                    Span::styled("Disconnect from ", st(IVORY)),
                    Span::styled(alias.clone(), st(tab_for(alias)).add_modifier(Modifier::BOLD)),
                    Span::styled("?", st(IVORY)),
                ]),
                inner.width,
            );
            put(
                buf,
                inner.x,
                inner.y + 1,
                Line::styled("Anything running in the shell stops.", st(GRAIN)),
                inner.width,
            );
            actions(
                buf,
                app,
                inner.x,
                inner.bottom() - 1,
                inner.width,
                &[("y", "disconnect", BRASS), ("n", "keep", BRASS)],
            );
        }
    }
}

// --------------------------------------------------------------- help card

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Everywhere",
        &[
            ("alt+1 2 3", "focus machines, shell, files"),
            ("tab", "next pane (outside the shell)"),
            ("alt+z", "zoom the shell to full window"),
            ("alt+q", "quit"),
            ("click", "focus a pane, pick a row"),
            ("double-click", "connect, open, preview"),
        ],
    ),
    (
        "Machines",
        &[
            ("j k  /", "move, search"),
            ("enter", "connect or switch"),
            ("a  e  D", "add, edit, delete machine"),
            ("f", "forward a port through it"),
            ("K", "copy key: no more passwords"),
            ("x  r", "close session, reload ssh config"),
        ],
    ),
    (
        "Shell",
        &[
            ("wheel", "scroll back, or the program"),
            ("alt+pgup pgdn", "scroll back a page"),
            ("↑ ↓  esc", "scrolled: a line, back to live"),
            ("drag", "select and copy"),
            ("double-click", "copy a word (triple: the line)"),
            ("shift+drag", "your terminal's own selection"),
            ("", "other keys go to the machine"),
        ],
    ),
    (
        "Files",
        &[
            ("j k  wheel", "move"),
            ("enter  h  ~", "open or preview, go up, home"),
            ("d", "download to ~/Downloads"),
            ("u", "upload from this computer"),
            ("p", "preview a text file"),
            ("/  s", "filter, sort by name, date, size"),
            ("n  R  D", "new folder, rename, delete"),
            ("y  c", "copy path, cd the shell here"),
            (".  r", "show hidden, refresh"),
        ],
    ),
];

fn help_rows(sections: &[(&str, &[(&str, &str)])]) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for (i, (section, keys)) in sections.iter().enumerate() {
        if i > 0 {
            rows.push(Line::raw(""));
        }
        rows.push(Line::styled(section.to_string(), st(IVORY).add_modifier(Modifier::BOLD)));
        for (k, l) in *keys {
            rows.push(Line::from(vec![
                Span::styled(format!("  {k:<15}"), st(BRASS).add_modifier(Modifier::BOLD)),
                Span::styled(l.to_string(), st(GRAIN)),
            ]));
        }
    }
    rows
}

fn draw_help(f: &mut Frame, area: Rect, app: &App, scroll: u16) {
    // Two columns when there's room, else one scrolling column.
    let two = area.width >= 116;
    let columns: Vec<Vec<Line>> =
        if two { vec![help_rows(&HELP[..2]), help_rows(&HELP[2..])] } else { vec![help_rows(HELP)] };
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(0) as u16;
    let w = if two { 112 } else { 64 };
    // Border and padding (4), the rows, a blank row, the action row.
    let inner = card(f, app, area, w, tallest + 6, "keys");
    let buf = f.buffer_mut();
    let body_h = inner.height.saturating_sub(2);
    let scroll = scroll.min(tallest.saturating_sub(body_h));
    let col_w = if two { (inner.width - 4) / 2 } else { inner.width };
    for (c, rows) in columns.into_iter().enumerate() {
        let x = inner.x + c as u16 * (col_w + 4);
        for (i, line) in rows.into_iter().skip(scroll as usize).take(body_h as usize).enumerate() {
            put(buf, x, inner.y + i as u16, line, col_w);
        }
    }
    let more = tallest > body_h;
    let mut hint = vec![("w", "welcome tour", BRASS)];
    if more {
        hint.push(("j", "more", BRASS));
    }
    actions(buf, app, inner.x, inner.bottom() - 1, inner.width, &hint);
    let note = Line::styled("any other key closes", meta());
    let nw = note.width() as u16;
    if nw + 30 < inner.width {
        put(buf, inner.right() - nw, inner.bottom() - 1, note, nw);
    }
}

// ------------------------------------------------------------ preview card

fn draw_preview(f: &mut Frame, area: Rect, app: &App, p: &Preview) {
    let w = area.width.saturating_sub(8).clamp(40, 132);
    let h = area.height.saturating_sub(4).max(10);
    let inner = card(f, app, area, w, h, "preview");
    let buf = f.buffer_mut();
    let tw = inner.width as usize;
    let hue = tab_for(&p.alias);
    put(
        buf,
        inner.x,
        inner.y,
        Line::from(vec![
            Span::styled(
                clip(&safe(&p.name), tw.saturating_sub(p.alias.len() + 18)),
                st(IVORY).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", human_size(p.size)), st(GRAIN)),
            Span::styled("  on  ", meta()),
            Span::styled(p.alias.clone(), st(hue).add_modifier(Modifier::BOLD)),
        ]),
        inner.width,
    );
    put(buf, inner.x, inner.y + 1, Line::styled(clip_left(&safe(&p.path), tw), meta()), inner.width);
    for x in inner.left()..inner.right() {
        buf[(x, inner.y + 2)].set_symbol("─").set_fg(RULE);
    }
    let text = Rect::new(inner.x, inner.y + 3, inner.width, inner.height.saturating_sub(5));
    p.page.set(text.height as usize);
    let mut position = None;
    match &p.body {
        PreviewBody::Loading => put(buf, text.x, text.y, Line::styled("Reading…", st(GRAIN)), text.width),
        PreviewBody::Failed(e) => {
            put(
                buf,
                text.x,
                text.y,
                Line::styled("Couldn't read this file", st(ERR).add_modifier(Modifier::BOLD)),
                text.width,
            );
            put(buf, text.x, text.y + 1, Line::styled(clip(&safe(e), tw), st(GRAIN)), text.width);
        }
        PreviewBody::Binary => {
            put(
                buf,
                text.x,
                text.y,
                Line::styled("This looks like a binary file.", st(IVORY).add_modifier(Modifier::BOLD)),
                text.width,
            );
            put(buf, text.x, text.y + 1, Line::styled("Download it with d to open it here.", st(GRAIN)), text.width);
        }
        PreviewBody::Text(lines, _) if lines.is_empty() => {
            put(buf, text.x, text.y, Line::styled("This file is empty.", st(GRAIN)), text.width);
        }
        PreviewBody::Text(lines, more) => {
            let digits = lines.len().to_string().len().max(2);
            let gutter = digits as u16 + 2;
            let body_w = text.width.saturating_sub(gutter) as usize;
            let first = p.scroll.min(lines.len().saturating_sub(1));
            for (i, line) in lines.iter().enumerate().skip(first).take(text.height as usize) {
                let y = text.y + (i - first) as u16;
                put(buf, text.x, y, Line::styled(format!("{:>digits$}", i + 1), meta()), gutter);
                put(buf, text.x + gutter, y, Line::styled(clip(line, body_w), st(IVORY)), text.width - gutter);
            }
            let last = (first + text.height as usize).min(lines.len());
            let mut pos = format!("{}–{} of {}", first + 1, last, lines.len());
            if *more {
                pos = format!("{pos} · first {} of {}", human_size(PREVIEW_LIMIT as u64), human_size(p.size));
            }
            position = Some(pos);
        }
    }
    let row = inner.bottom() - 1;
    actions(
        buf,
        app,
        inner.x,
        row,
        inner.width,
        &[("d", "download", BRASS), ("y", "copy path", BRASS), ("esc", "close", BRASS)],
    );
    if let Some(pos) = position {
        let line = Line::styled(pos, meta());
        let lw = line.width() as u16;
        if lw + 50 < inner.width {
            put(buf, inner.right() - lw, row, line, lw);
        }
    }
}

// --------------------------------------------------------------- name card

fn draw_name(f: &mut Frame, area: Rect, app: &App, p: &NameInput) {
    let (title, verb) = match &p.op {
        NameOp::Mkdir => ("new folder", "create"),
        NameOp::Rename(_) => ("rename", "rename"),
    };
    let inner = card(f, app, area, 60, 11, title);
    let buf = f.buffer_mut();
    let w = inner.width as usize;
    let mut head = vec![Span::styled(p.alias.clone(), st(tab_for(&p.alias)).add_modifier(Modifier::BOLD))];
    head.push(Span::styled(format!(":{}", clip_left(&safe(&p.dir), w.saturating_sub(p.alias.len() + 2))), st(IVORY)));
    put(buf, inner.x, inner.y, Line::from(head), inner.width);
    if let NameOp::Rename(old) = &p.op {
        put(
            buf,
            inner.x,
            inner.y + 1,
            Line::styled(format!("was {}", clip(&safe(old), w.saturating_sub(4))), meta()),
            inner.width,
        );
    }
    put(
        buf,
        inner.x,
        inner.y + 3,
        Line::from(vec![
            Span::styled(clip_left(&p.input, w.saturating_sub(2)), st(IVORY)),
            Span::styled("▏", st(BRASS)),
        ]),
        inner.width,
    );
    for x in inner.left()..inner.right() {
        buf[(x, inner.y + 4)].set_symbol("─").set_fg(BRASS);
    }
    if let Some(e) = &p.error {
        put(buf, inner.x, inner.y + 5, Line::styled(e.clone(), st(ERR).add_modifier(Modifier::BOLD)), inner.width);
    }
    actions(buf, app, inner.x, inner.bottom() - 1, inner.width, &[("enter", verb, BRASS), ("esc", "cancel", BRASS)]);
}

// ------------------------------------------------------------ forwards card

fn forward_line(fwd: &Forward, sel: bool, focused: bool) -> Line<'static> {
    let port_style =
        if sel && focused { st(BRASS).add_modifier(Modifier::BOLD) } else { st(IVORY).add_modifier(Modifier::BOLD) };
    let quiet = if sel { st(GRAIN) } else { st(FAINT) };
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("localhost:{:<5}", fwd.port()), port_style),
        Span::styled("  →  ", quiet),
        Span::styled(fwd.target(), st(GRAIN)),
    ])
}

fn draw_forwards(f: &mut Frame, area: Rect, app: &App, p: &Forwards) {
    let list: Vec<Forward> = app.sessions.get(&p.alias).map(|s| s.forwards.clone()).unwrap_or_default();
    let rows = list.len().max(1) as u16;
    let typing = p.input.is_some();
    let h = 8
        + rows
        + if typing {
            6
        } else if p.error.is_some() {
            2
        } else {
            0
        };
    let inner = card(f, app, area, 66, h, "forward ports");
    let buf = f.buffer_mut();
    let hue = tab_for(&p.alias);
    put(
        buf,
        inner.x,
        inner.y,
        Line::from(vec![
            Span::styled("Through  ", st(GRAIN)),
            Span::styled(p.alias.clone(), st(hue).add_modifier(Modifier::BOLD)),
            Span::styled("  ·  only this computer can use them", meta()),
        ]),
        inner.width,
    );
    let top = inner.y + 2;
    if list.is_empty() {
        put(buf, inner.x + 1, top, Line::styled("No ports forwarded yet.", st(GRAIN)), inner.width);
    }
    for (i, fwd) in list.iter().enumerate() {
        let y = top + i as u16;
        let sel = i == p.selected && !typing;
        if sel {
            fill(buf, Rect::new(inner.x, y, inner.width, 1), ROW_SEL);
        }
        put(buf, inner.x, y, forward_line(fwd, sel, !typing), inner.width);
    }
    let y = top + rows + 1;
    if let Some(input) = &p.input {
        put(buf, inner.x, y, Line::styled("New forward", st(BRASS).add_modifier(Modifier::BOLD)), inner.width);
        put(
            buf,
            inner.x,
            y + 1,
            Line::from(vec![
                Span::styled(clip_left(input, inner.width.saturating_sub(2) as usize), st(IVORY)),
                Span::styled("▏", st(BRASS)),
                Span::styled(if input.is_empty() { "a port, like 3000" } else { "" }, meta()),
            ]),
            inner.width,
        );
        for x in inner.left()..inner.right() {
            buf[(x, y + 2)].set_symbol("─").set_fg(BRASS);
        }
        // Say in words what the typed spec will do, or why it won't.
        let explain = match (&p.error, Forward::parse(input)) {
            (Some(e), _) => Line::styled(clip(&safe(e), inner.width as usize), st(ERR).add_modifier(Modifier::BOLD)),
            (None, _) if input.trim().is_empty() => Line::raw(""),
            (None, Ok(Forward::Socks { port })) => Line::from(vec![
                Span::styled(format!("A SOCKS proxy on localhost:{port} that goes out through "), st(GRAIN)),
                Span::styled(p.alias.clone(), st(hue)),
            ]),
            (None, Ok(fwd)) => Line::from(vec![
                Span::styled(format!("localhost:{}", fwd.port()), st(IVORY).add_modifier(Modifier::BOLD)),
                Span::styled(" here reaches ", st(GRAIN)),
                Span::styled(fwd.target(), st(IVORY).add_modifier(Modifier::BOLD)),
                Span::styled(" on ", st(GRAIN)),
                Span::styled(p.alias.clone(), st(hue)),
            ]),
            (None, Err(e)) => Line::styled(clip(&e, inner.width as usize), meta()),
        };
        put(buf, inner.x, y + 3, explain, inner.width);
        put(
            buf,
            inner.x,
            y + 4,
            Line::styled("3000  ·  8080:3000  ·  8080:10.0.0.5:80  ·  socks 1080", meta()),
            inner.width,
        );
        let back = if list.is_empty() { "cancel" } else { "back" };
        actions(
            buf,
            app,
            inner.x,
            inner.bottom() - 1,
            inner.width,
            &[("enter", "forward", BRASS), ("esc", back, BRASS)],
        );
    } else {
        if let Some(e) = &p.error {
            put(
                buf,
                inner.x,
                y,
                Line::styled(clip(&safe(e), inner.width as usize), st(ERR).add_modifier(Modifier::BOLD)),
                inner.width,
            );
        }
        let can_open = matches!(list.get(p.selected), Some(Forward::Local { .. }));
        let can_stop = !list.is_empty();
        actions(
            buf,
            app,
            inner.x,
            inner.bottom() - 1,
            inner.width,
            &[
                ("n", "new", BRASS),
                ("o", "open in browser", if can_open { BRASS } else { FAINT }),
                ("x", "stop", if can_stop { BRASS } else { FAINT }),
                ("esc", "close", BRASS),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_map_to_keys() {
        assert_eq!(hint_key("enter"), Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(hint_key("alt+z"), Some(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)));
        assert_eq!(hint_key("alt+pgup"), Some(KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT)));
        assert_eq!(hint_key("ctrl+s"), Some(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert_eq!(hint_key("D"), Some(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)));
        assert_eq!(hint_key("↑↓"), None);
        assert_eq!(hint_key("wheel"), None);
    }

    #[test]
    fn zoom_gives_the_shell_the_window() {
        let area = Rect::new(0, 0, 160, 40);
        let normal = layout(area, Focus::Shell, false);
        assert!(normal.files.is_some() && normal.machines.width > 0);
        let zoomed = layout(area, Focus::Shell, true);
        assert_eq!(zoomed.shell, Some(Rect::new(0, 0, 160, 39)));
        assert!(zoomed.files.is_none());
        assert_eq!(zoomed.machines.width, 0);
    }
}
