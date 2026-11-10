//! Rendering. Oak cabinet, catalog-card panes, brass only on what you can act on.

use chrono::{Local, NaiveDate, TimeZone};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};
use tui_term::widget::{Cursor, PseudoTerminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, FORM_HINTS, FORM_LABELS, Focus, Panes, Popup, Tone, Transfer};
use crate::download;
use crate::remote::Kind;
use crate::session::State;
use crate::theme::*;

// ------------------------------------------------------------------ layout

pub fn layout(area: Rect, focus: Focus) -> Panes {
    let mut p = Panes::default();
    if area.width < 60 || area.height < 14 {
        p.too_small = true;
        return p;
    }
    let body_h = area.height - 1;
    p.footer = Rect::new(area.x, area.y + body_h, area.width, 1);
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
    draw_machines(f.buffer_mut(), p.machines, app);
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
    let right = match session {
        Some(s) if s.scroll > 0 => vec![Span::styled(format!("↑ {} lines", s.scroll), st(GRAIN))],
        _ => vec![],
    };
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
        if focused && s.is_live() && s.scroll == 0 && !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            if row < inner.height && col < inner.width {
                f.set_cursor_position((inner.x + col, inner.y + row));
            }
        }
    }

    if !s.is_live() {
        let failed = matches!(s.state, State::Failed(_));
        let mut spans = vec![Span::styled(
            if failed { " Couldn't connect. " } else { " Session ended. " },
            st(if failed { ERR } else { IVORY }).add_modifier(Modifier::BOLD),
        )];
        spans.push(Span::raw("  "));
        spans.extend(key_hint("enter", if failed { "try again" } else { "reconnect" }));
        spans.push(Span::raw("   "));
        spans.extend(key_hint("x", "close"));
        let bar = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let buf = f.buffer_mut();
        fill(buf, bar, CARD);
        for x in bar.left()..bar.right() {
            buf[(x, bar.y)].set_symbol(" ");
        }
        put(buf, bar.x, bar.y, Line::from(spans), bar.width);
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

    let hidden = files.hidden_count();
    let mut status =
        vec![Span::styled(format!("{} item{}", visible.len(), if visible.len() == 1 { "" } else { "s" }), st(FAINT))];
    if hidden > 0 {
        status.push(Span::styled(format!(" · {hidden} hidden "), st(FAINT)));
        status.push(Span::styled(".", st(BRASS).add_modifier(Modifier::BOLD)));
    }
    if files.loading && !files.entries.is_empty() {
        status.push(Span::styled(" · reading…", st(FAINT)));
    }
    put(buf, inner.x + 1, inner.bottom() - 1, Line::from(status), inner.width - 1);
}

fn draw_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let live = app.current_session().is_some_and(|s| s.is_live());
    let pairs: &[(&str, &str)] = match app.focus {
        Focus::Machines if app.hosts.is_empty() => &[("a", "add"), ("r", "reload"), ("?", "help"), ("q", "quit")],
        Focus::Machines if app.searching => &[("enter", "connect"), ("esc", "clear"), ("↑↓", "move")],
        Focus::Machines => &[
            ("enter", "connect"),
            ("a", "add"),
            ("e", "edit"),
            ("D", "delete"),
            ("K", "copy key"),
            ("/", "search"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Focus::Shell if live => {
            &[("alt+1", "machines"), ("alt+3", "files"), ("shift+pgup", "scroll back"), ("alt+q", "quit")]
        }
        Focus::Shell => &[("enter", "connect"), ("tab", "next pane"), ("?", "help"), ("q", "quit")],
        Focus::Files => {
            &[("enter", "open"), ("h", "back"), ("d", "download"), ("u", "upload"), (".", "hidden"), ("?", "help")]
        }
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
        let gw: usize = g.iter().map(|s| s.width()).sum::<usize>() + if i > 0 { gap } else { 0 };
        if used + gw + right_w > area.width as usize {
            break;
        }
        if i > 0 {
            left.push(Span::raw(" ".repeat(gap)));
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

fn card(f: &mut Frame, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
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
            let inner = card(f, area, 60, 13, if ask.is_dir { "download folder" } else { "download" });
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
            let mut keys = key_hint("y", "download");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("n", "cancel"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::Transfer(id) => {
            let Some(t) = app.transfers.iter().find(|t| t.id == *id) else { return };
            let inner = card(f, area, 60, 13, if t.upload { "uploading" } else { "downloading" });
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
            let mut keys = key_hint("c", "cancel");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("esc", "hide"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::Help => {
            let rows: &[(&str, &[(&str, &str)])] = &[
                (
                    "Everywhere",
                    &[
                        ("alt+1 2 3", "focus machines, shell, files"),
                        ("tab", "next pane (outside the shell)"),
                        ("alt+q", "quit"),
                    ],
                ),
                (
                    "Machines",
                    &[
                        ("j k  /", "move, search"),
                        ("enter", "connect or switch"),
                        ("a  e  D", "add, edit, delete machine"),
                        ("K", "copy your SSH key (no more passwords)"),
                        ("x", "close session"),
                        ("r", "reload ssh config"),
                    ],
                ),
                ("Shell", &[("shift+pgup/pgdn", "scroll back"), ("", "every other key goes to the machine")]),
                (
                    "Files",
                    &[
                        ("j k", "move"),
                        ("enter l  h  ~", "open folder, go up, home"),
                        ("d", "download file or folder to ~/Downloads"),
                        ("u", "upload from this computer"),
                        (".  r", "show hidden, refresh"),
                    ],
                ),
            ];
            let h = rows.iter().map(|(_, r)| r.len() + 2).sum::<usize>() as u16 + 5;
            let inner = card(f, area, 66, h, "keys");
            let buf = f.buffer_mut();
            let mut y = inner.y;
            for (section, keys) in rows {
                put(buf, inner.x, y, Line::styled(*section, st(IVORY).add_modifier(Modifier::BOLD)), inner.width);
                y += 1;
                for (k, l) in *keys {
                    put(
                        buf,
                        inner.x,
                        y,
                        Line::from(vec![
                            Span::styled(format!("  {k:<18}"), st(BRASS).add_modifier(Modifier::BOLD)),
                            Span::styled(*l, st(GRAIN)),
                        ]),
                        inner.width,
                    );
                    y += 1;
                }
                y += 1;
            }
            let mut close = key_hint("w", "welcome tour");
            close.push(Span::styled("   any other key closes", meta()));
            put(buf, inner.x, inner.bottom() - 1, Line::from(close), inner.width);
        }
        Popup::HostForm(form) => {
            let title = if form.editing.is_some() { "edit machine" } else { "add machine" };
            let inner = card(f, area, 64, 19, title);
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
            let mut keys = key_hint("tab", "next field");
            keys.push(Span::raw("    "));
            keys.extend(key_hint("ctrl+s", "save"));
            keys.push(Span::raw("    "));
            keys.extend(key_hint("esc", "cancel"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::ConfirmDelete { alias, file } => {
            let inner = card(f, area, 60, 10, "delete machine");
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
            let mut keys =
                vec![Span::styled("y", st(ERR).add_modifier(Modifier::BOLD)), Span::styled(" delete", st(GRAIN))];
            keys.push(Span::raw("     "));
            keys.extend(key_hint("n", "keep"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::CopyKey { alias, key_path, key } => {
            let inner = card(f, area, 64, 11, "copy key");
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
            let mut keys = key_hint("y", "copy key");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("n", "cancel"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::Upload(p) => {
            let inner = card(f, area, 68, 16, "upload");
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
            let mut keys = key_hint("tab", "complete");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("enter", "upload"));
            keys.push(Span::raw("     "));
            keys.extend(key_hint("esc", "cancel"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::ConfirmQuit(n) => {
            let inner = card(f, area, 48, 8, "quit");
            let buf = f.buffer_mut();
            let s = if *n == 1 { "1 session is open." } else { "" };
            let text = if s.is_empty() { format!("{n} sessions are open.") } else { s.to_string() };
            put(buf, inner.x, inner.y, Line::styled(text, st(IVORY).add_modifier(Modifier::BOLD)), inner.width);
            put(buf, inner.x, inner.y + 1, Line::styled("Quitting closes them.", st(GRAIN)), inner.width);
            let mut keys = key_hint("y", "quit");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("n", "stay"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
        Popup::ConfirmClose(alias) => {
            let inner = card(f, area, 48, 8, "close session");
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
            let mut keys = key_hint("y", "disconnect");
            keys.push(Span::raw("     "));
            keys.extend(key_hint("n", "keep"));
            put(buf, inner.x, inner.bottom() - 1, Line::from(keys), inner.width);
        }
    }
}
