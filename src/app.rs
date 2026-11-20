//! Application state and input handling.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::layout::{Position, Rect};
use russh_sftp::client::SftpSession;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::clipboard;
use crate::configedit::{self, HostSpec};
use crate::download;
use crate::hosts::{self, Host, Scan};
use crate::keys;
use crate::mouse::{self, Clicks, Selection};
use crate::onboarding::{self, Onboarding};
use crate::remote::{self, Entry, Forward, Kind};
use crate::session::{self, Session, State};
use crate::transfer::{self, Progress};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Machines,
    Shell,
    Files,
}

pub enum Msg {
    Output,
    HostsLoaded(Scan),
    HostResolved(Host),
    SftpReady {
        session: u64,
        sftp: Arc<SftpSession>,
        home: String,
    },
    SftpFailed {
        session: u64,
        error: String,
    },
    Listing {
        session: u64,
        dir: String,
        result: Result<Vec<Entry>, String>,
    },
    Transfer {
        id: u64,
        progress: Progress,
    },
    KeyInstalled {
        alias: String,
        result: Result<bool, String>,
    },
    /// A new folder, rename or delete finished. Ok carries the toast text.
    FileOp {
        session: u64,
        dir: String,
        select: Option<String>,
        result: Result<String, String>,
    },
    Preview {
        session: u64,
        path: String,
        result: Result<(Vec<u8>, bool), String>,
    },
    Forward {
        session: u64,
        fwd: Forward,
        cancel: bool,
        result: Result<(), String>,
    },
}

pub struct DownloadAsk {
    pub alias: String,
    pub remote_path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub target: PathBuf,
}

pub const FORM_LABELS: [&str; 5] = ["Name", "Address", "User", "Port", "Key file"];
pub const FORM_HINTS: [&str; 5] = [
    "short name, e.g. proxmox",
    "IP or hostname",
    "optional",
    "optional, default 22",
    "optional, e.g. ~/.ssh/id_ed25519",
];

pub struct Form {
    /// Alias being edited; None when adding.
    pub editing: Option<String>,
    /// Config file the machine is written to.
    pub file: PathBuf,
    pub fields: [String; 5],
    pub focus: usize,
    pub error: Option<String>,
}

impl Form {
    fn spec(&self) -> HostSpec {
        let [alias, hostname, user, port, identity_file] = self.fields.clone();
        HostSpec { alias, hostname, user, port, identity_file }
    }
}

pub struct PathInput {
    pub alias: String,
    pub remote_dir: String,
    pub input: String,
    pub suggestions: Vec<String>,
    pub error: Option<String>,
}

/// Bytes of a file read for preview.
pub const PREVIEW_LIMIT: usize = 512 * 1024;

pub enum PreviewBody {
    Loading,
    /// Lines ready to draw, and whether the file goes on past what was read.
    Text(Vec<String>, bool),
    Binary,
    Failed(String),
}

pub struct Preview {
    pub alias: String,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub body: PreviewBody,
    pub scroll: usize,
    /// Text rows on screen, written while drawing (for paging).
    pub page: Cell<usize>,
}

impl Preview {
    fn max_scroll(&self) -> usize {
        match &self.body {
            PreviewBody::Text(lines, _) => lines.len().saturating_sub(self.page.get().max(1)),
            _ => 0,
        }
    }
    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(self.max_scroll());
    }
}

/// Turn the start of a file into lines for the preview, or None if it looks
/// binary. Tabs become spaces; control characters can't reach the screen.
pub fn preview_lines(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    Some(
        text.lines()
            .map(|l| {
                let mut out = String::with_capacity(l.len());
                for c in l.chars() {
                    match c {
                        '\t' => out.push_str(&" ".repeat(4 - out.chars().count() % 4)),
                        c if c.is_control() => out.push('·'),
                        c => out.push(c),
                    }
                }
                out
            })
            .collect(),
    )
}

#[derive(Clone, PartialEq, Eq)]
pub enum NameOp {
    Mkdir,
    Rename(String),
}

/// One-line name entry for a new folder or a rename.
pub struct NameInput {
    pub alias: String,
    pub dir: String,
    pub op: NameOp,
    pub input: String,
    pub error: Option<String>,
}

/// The port forwards card. `input` is Some while typing a new forward.
pub struct Forwards {
    pub alias: String,
    pub selected: usize,
    pub input: Option<String>,
    pub error: Option<String>,
}

pub enum Popup {
    Download(DownloadAsk),
    Transfer(u64),
    /// Scroll offset, for short windows.
    Help(u16),
    Preview(Preview),
    Name(NameInput),
    ConfirmRemove {
        alias: String,
        dir: String,
        name: String,
        is_dir: bool,
    },
    Forwards(Forwards),
    ConfirmQuit(usize),
    ConfirmClose(String),
    HostForm(Form),
    ConfirmDelete {
        alias: String,
        file: PathBuf,
    },
    CopyKey {
        alias: String,
        key_path: PathBuf,
        key: String,
    },
    Upload(PathInput),
}

pub struct Transfer {
    pub id: u64,
    pub alias: String,
    pub remote_path: String,
    pub name: String,
    pub upload: bool,
    pub is_dir: bool,
    pub total: u64,
    pub done: u64,
    pub file: String,
    pub started: Instant,
    pub cancel: Arc<AtomicBool>,
    pub stamped: NaiveDate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Good,
    Bad,
}

pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pub until: Instant,
}

/// Something clickable, recorded while drawing so clicks land on exactly what
/// is on screen.
#[derive(Clone, Copy)]
pub enum Hit {
    Machine(usize),
    File(usize),
    /// A key hint: clicking it presses the key.
    Key(KeyEvent),
    /// A popup card's area (clicks outside it may close it).
    Card,
}

/// A mouse drag that started in the shell and follows the pointer anywhere.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grab {
    /// Selecting text.
    Select,
    /// Reporting to the remote program (it asked for mouse events).
    Forward,
}

#[derive(Default, Clone, Copy)]
pub struct Panes {
    pub machines: Rect,
    pub shell: Option<Rect>,
    pub files: Option<Rect>,
    pub footer: Rect,
    pub too_small: bool,
}

pub struct App {
    tx: UnboundedSender<Msg>,
    pub hosts: Vec<Host>,
    pub hosts_loaded: bool,
    /// Every (alias, file) from the last config scan, for choosing where new
    /// machines go.
    scan_hosts: Vec<(String, PathBuf)>,
    pub selected: usize,
    pub focus: Focus,
    pub filter: String,
    pub searching: bool,
    pending_select: Option<String>,
    pub sessions: HashMap<String, Session>,
    /// Machines whose session failed or dropped; held until you look at them.
    pub attention: HashSet<String>,
    pub popup: Option<Popup>,
    pub transfers: Vec<Transfer>,
    /// (alias, remote path) -> date stamped. Shown in the file list.
    pub stamps: HashMap<(String, String), NaiveDate>,
    pub toast: Option<Toast>,
    pub quit: bool,
    pub panes: Panes,
    pub downloads_dir: PathBuf,
    pub onboarding: Option<Onboarding>,
    /// The shell fills the window (alt+z).
    pub zoom: bool,
    /// Text selected in the shell pane with the mouse.
    pub selection: Option<Selection>,
    /// Clickable regions from the last frame, in drawing order.
    pub hits: RefCell<Vec<(Rect, Hit)>>,
    clicks: Clicks,
    grab: Option<Grab>,
    socket_dir: Result<PathBuf, String>,
    next_id: u64,
}

impl App {
    pub fn new(tx: UnboundedSender<Msg>, welcome: bool) -> Self {
        let downloads_dir = download::downloads_dir();
        let onboarding =
            welcome.then(|| Onboarding { step: 0, setup: onboarding::Setup::detect(downloads_dir.clone()) });
        Self {
            tx,
            hosts: Vec::new(),
            hosts_loaded: false,
            scan_hosts: Vec::new(),
            selected: 0,
            focus: Focus::Machines,
            filter: String::new(),
            searching: false,
            pending_select: None,
            sessions: HashMap::new(),
            attention: HashSet::new(),
            popup: None,
            transfers: Vec::new(),
            stamps: HashMap::new(),
            toast: None,
            quit: false,
            panes: Panes::default(),
            downloads_dir,
            onboarding,
            zoom: false,
            selection: None,
            hits: RefCell::new(Vec::new()),
            clicks: Clicks::default(),
            grab: None,
            socket_dir: session::runtime_dir().map_err(|e| format!("{e:#}")),
            next_id: 1,
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn current_host(&self) -> Option<&Host> {
        self.hosts.get(self.selected).filter(|_| self.visible_hosts().contains(&self.selected))
    }

    pub fn current_session(&self) -> Option<&Session> {
        self.current_host().and_then(|h| self.sessions.get(&h.alias))
    }

    fn current_session_mut(&mut self) -> Option<&mut Session> {
        let alias = self.current_host()?.alias.clone();
        self.sessions.get_mut(&alias)
    }

    /// Indices of hosts matching the search filter, in order.
    pub fn visible_hosts(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        self.hosts
            .iter()
            .enumerate()
            .filter(|(_, h)| {
                q.is_empty()
                    || h.alias.to_lowercase().contains(&q)
                    || h.hostname.as_deref().is_some_and(|n| n.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let vis = self.visible_hosts();
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == self.selected).unwrap_or(0) as isize;
        let next = (pos + delta).clamp(0, vis.len() as isize - 1) as usize;
        self.selected = vis[next];
    }

    fn snap_selection(&mut self) {
        let vis = self.visible_hosts();
        if !vis.contains(&self.selected)
            && let Some(&first) = vis.first()
        {
            self.selected = first;
        }
    }

    pub fn live_sessions(&self) -> usize {
        self.sessions.values().filter(|s| s.is_live()).count()
    }

    fn toast(&mut self, text: impl Into<String>, tone: Tone) {
        self.toast = Some(Toast { text: text.into(), tone, until: Instant::now() + Duration::from_secs(6) });
    }

    /// Record a clickable region (called while drawing).
    pub fn hit(&self, area: Rect, hit: Hit) {
        self.hits.borrow_mut().push((area, hit));
    }

    /// The topmost hit under (x, y). With a popup open, only the popup's own
    /// hits count.
    fn hit_at(&self, x: u16, y: u16) -> Option<Hit> {
        let hits = self.hits.borrow();
        let start = if self.popup.is_some() {
            hits.iter().rposition(|(_, h)| matches!(h, Hit::Card)).unwrap_or(hits.len())
        } else {
            0
        };
        hits[start..].iter().rev().find(|(r, _)| r.contains(Position { x, y })).map(|(_, h)| *h)
    }

    // ---------------------------------------------------------------- hosts

    pub fn load_hosts(&mut self) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::HostsLoaded(hosts::scan()));
        });
    }

    fn on_hosts(&mut self, scan: Scan) {
        let keep = self.pending_select.take().or_else(|| self.current_host().map(|h| h.alias.clone()));
        let old: HashMap<String, Host> = self.hosts.drain(..).map(|h| (h.alias.clone(), h)).collect();
        self.hosts = scan
            .hosts
            .iter()
            .map(|(a, file)| {
                let mut h = old.get(a).cloned().unwrap_or_else(|| Host::new(a.clone()));
                h.source = Some(file.clone());
                h
            })
            .collect();
        // Sessions for machines removed from the config stay listed until closed.
        for alias in self.sessions.keys() {
            if !self.hosts.iter().any(|h| &h.alias == alias) {
                self.hosts.push(Host::new(alias.clone()));
            }
        }
        self.selected = keep
            .and_then(|k| self.hosts.iter().position(|h| h.alias == k))
            .unwrap_or(0)
            .min(self.hosts.len().saturating_sub(1));
        self.snap_selection();
        self.hosts_loaded = true;
        for (a, _) in &scan.hosts {
            let tx = self.tx.clone();
            let a = a.clone();
            tokio::spawn(async move {
                let _ = tx.send(Msg::HostResolved(hosts::resolve(&a).await));
            });
        }
        self.scan_hosts = scan.hosts;
    }

    fn open_add_form(&mut self) {
        let file = configedit::add_target(&self.scan_hosts);
        self.popup =
            Some(Popup::HostForm(Form { editing: None, file, fields: Default::default(), focus: 0, error: None }));
    }

    fn open_edit_form(&mut self) {
        let Some(h) = self.current_host().cloned() else { return };
        let Some(file) = h.source.clone() else {
            return self.toast(format!("{} isn't in a config file", h.alias), Tone::Info);
        };
        if configedit::is_shared(&file, &h.alias) {
            return self.toast(
                format!("{} shares a Host line with other machines; edit {} by hand", h.alias, download::tilde(&file)),
                Tone::Info,
            );
        }
        match configedit::read_spec(&file, &h.alias) {
            Ok(s) => {
                self.popup = Some(Popup::HostForm(Form {
                    editing: Some(h.alias.clone()),
                    file,
                    fields: [s.alias, s.hostname, s.user, s.port, s.identity_file],
                    focus: 1,
                    error: None,
                }))
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
    }

    fn save_form(&mut self, mut form: Form) {
        let spec = match form.spec().validated() {
            Ok(s) => s,
            Err(e) => {
                form.error = Some(format!("{e:#}"));
                self.popup = Some(Popup::HostForm(form));
                return;
            }
        };
        if let Some(old) = &form.editing {
            if old != &spec.alias && self.sessions.get(old).is_some_and(|s| s.is_live()) {
                form.error = Some(format!("Disconnect {old} before renaming it"));
                self.popup = Some(Popup::HostForm(form));
                return;
            }
        } else if self.hosts.iter().any(|h| h.alias == spec.alias) {
            form.error = Some(format!("A machine named {} already exists", spec.alias));
            self.popup = Some(Popup::HostForm(form));
            return;
        }
        let result = match &form.editing {
            Some(old) => configedit::modify(&form.file, |t| configedit::edit_in(t, old, &spec)),
            None => configedit::modify(&form.file, |t| configedit::add_to(t, &spec)),
        };
        match result {
            Ok(real) => {
                let verb = if form.editing.is_some() { "Updated" } else { "Added" };
                self.toast(format!("{verb} {} in {}", spec.alias, download::tilde(&real)), Tone::Good);
                self.filter.clear();
                self.pending_select = Some(spec.alias);
                self.load_hosts();
            }
            Err(e) => {
                form.error = Some(format!("{e:#}"));
                self.popup = Some(Popup::HostForm(form));
            }
        }
    }

    fn delete_host(&mut self, alias: &str, file: &std::path::Path) {
        if self.sessions.contains_key(alias) {
            self.close_session(alias);
        }
        match configedit::modify(file, |t| configedit::delete_in(t, alias)) {
            Ok(real) => {
                let name = real.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.toast(format!("Removed {alias}. Backup: {name}.drawer.bak"), Tone::Good);
                self.load_hosts();
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
    }

    fn ask_copy_key(&mut self) {
        let Some(h) = self.current_host().cloned() else { return };
        if self.sessions.get(&h.alias).map(|s| &s.state) != Some(&State::Connected) {
            return self.toast(format!("Connect to {} first (enter), then press K", h.alias), Tone::Info);
        }
        // Prefer the key this host is configured with, then the usual defaults.
        let mut candidates = Vec::new();
        if let Some(file) = &h.source
            && let Ok(spec) = configedit::read_spec(file, &h.alias)
            && !spec.identity_file.is_empty()
        {
            let p = hosts::expand(&spec.identity_file, &hosts::ssh_dir());
            candidates.push(PathBuf::from(format!("{}.pub", p.display())));
        }
        for n in ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"] {
            candidates.push(hosts::ssh_dir().join(n));
        }
        for p in candidates {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let key = text.lines().next().unwrap_or("").trim().to_string();
                if remote::valid_pubkey(&key) {
                    self.popup = Some(Popup::CopyKey { alias: h.alias, key_path: p, key });
                    return;
                }
            }
        }
        self.toast("No SSH key found. Create one with: ssh-keygen -t ed25519", Tone::Info);
    }

    fn copy_key(&mut self, alias: String, key: String) {
        let Some(path) = self.sessions.get(&alias).map(|s| s.control_path.clone()) else { return };
        let tx = self.tx.clone();
        self.toast(format!("Copying key to {alias}…"), Tone::Info);
        tokio::spawn(async move {
            let result = remote::install_key(&alias, &path, &key).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::KeyInstalled { alias, result });
        });
    }

    // ------------------------------------------------------------- sessions

    fn connect_selected(&mut self) {
        let Some(host) = self.current_host().cloned() else { return };
        self.attention.remove(&host.alias);
        if self.sessions.get(&host.alias).is_some_and(|s| s.is_live()) {
            self.focus = Focus::Shell;
            return;
        }
        let dir = match &self.socket_dir {
            Ok(d) => d.clone(),
            Err(e) => return self.toast(format!("Cannot connect: {e}"), Tone::Bad),
        };
        let (rows, cols) = self.shell_size();
        let id = self.id();
        let tx = self.tx.clone();
        match Session::spawn(id, &host.alias, rows, cols, &dir, move || {
            let _ = tx.send(Msg::Output);
        }) {
            Ok(s) => {
                self.sessions.insert(host.alias.clone(), s);
                self.focus = Focus::Shell;
            }
            Err(e) => self.toast(format!("{e:#}"), Tone::Bad),
        }
    }

    fn close_session(&mut self, alias: &str) {
        if let Some(mut s) = self.sessions.remove(alias) {
            s.kill();
        }
        for t in self.transfers.iter().filter(|t| t.alias == alias) {
            t.cancel.store(true, Ordering::Relaxed);
        }
        self.attention.remove(alias);
        if self.focus == Focus::Shell {
            self.focus = Focus::Machines;
        }
    }

    pub fn shutdown(&mut self) {
        for t in &self.transfers {
            t.cancel.store(true, Ordering::Relaxed);
        }
        for (_, mut s) in self.sessions.drain() {
            s.kill();
        }
    }

    fn shell_size(&self) -> (u16, u16) {
        match self.panes.shell {
            Some(r) => (r.height.saturating_sub(2), r.width.saturating_sub(2)),
            None => (24, 80),
        }
    }

    /// Called every frame with the current screen size.
    pub fn apply_area(&mut self, area: Rect) {
        // Zoom only makes sense while the shell has focus.
        if self.focus != Focus::Shell {
            self.zoom = false;
        }
        self.panes = ui::layout(area, self.focus, self.zoom);
        if let Some(r) = self.panes.shell {
            let (rows, cols) = (r.height.saturating_sub(2), r.width.saturating_sub(2));
            for s in self.sessions.values_mut() {
                s.resize(rows, cols);
            }
        }
    }

    fn start_sftp(&mut self, alias: &str) {
        let Some(s) = self.sessions.get_mut(alias) else { return };
        if s.state != State::Connected || s.files.sftp.is_some() || s.sftp_starting {
            return;
        }
        s.sftp_starting = true;
        s.files.loading = true;
        s.files.error = None;
        let (id, alias, path, tx) = (s.id, s.alias.clone(), s.control_path.clone(), self.tx.clone());
        tokio::spawn(async move {
            let msg = match remote::connect(&alias, &path).await {
                Ok((sftp, home)) => Msg::SftpReady { session: id, sftp, home },
                Err(e) => Msg::SftpFailed { session: id, error: format!("{e:#}") },
            };
            let _ = tx.send(msg);
        });
    }

    fn list_dir(&mut self, alias: &str, dir: String) {
        let Some(s) = self.sessions.get_mut(alias) else { return };
        let Some(sftp) = s.files.sftp.clone() else { return };
        s.files.loading = true;
        s.files.error = None;
        let (id, tx) = (s.id, self.tx.clone());
        tokio::spawn(async move {
            let result = remote::list(&sftp, &dir).await;
            let _ = tx.send(Msg::Listing { session: id, dir, result });
        });
    }

    fn session_by_id(&mut self, id: u64) -> Option<&mut Session> {
        self.sessions.values_mut().find(|s| s.id == id)
    }

    // ------------------------------------------------------------- messages

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Output => {}
            Msg::HostsLoaded(scan) => self.on_hosts(scan),
            Msg::HostResolved(h) => {
                if let Some(slot) = self.hosts.iter_mut().find(|x| x.alias == h.alias) {
                    let source = slot.source.take();
                    *slot = Host { source, ..h };
                }
            }
            Msg::SftpReady { session, sftp, home } => {
                let Some(s) = self.session_by_id(session) else { return };
                s.sftp_starting = false;
                s.files.sftp = Some(sftp);
                s.files.home = home.clone();
                let alias = s.alias.clone();
                self.list_dir(&alias, home);
            }
            Msg::SftpFailed { session, error } => {
                let Some(s) = self.session_by_id(session) else { return };
                s.sftp_starting = false;
                s.files.loading = false;
                s.files.error = Some(error);
            }
            Msg::Listing { session, dir, result } => {
                let Some(s) = self.session_by_id(session) else { return };
                let f = &mut s.files;
                f.loading = false;
                match result {
                    Ok(entries) => {
                        let same_dir = f.cwd == dir;
                        let prev = if same_dir { f.current().map(|e| e.name) } else { f.came_from.take() };
                        let prev = f.pending_select.take().or(prev);
                        if !same_dir {
                            f.filter.clear();
                            f.filtering = false;
                        }
                        f.cwd = dir;
                        f.entries = entries;
                        f.resort();
                        f.error = None;
                        f.selected = prev.and_then(|n| f.visible().iter().position(|e| e.name == n)).unwrap_or(0);
                    }
                    Err(e) => {
                        f.came_from = None;
                        f.error = Some(e);
                    }
                }
            }
            Msg::Transfer { id, progress } => self.on_transfer(id, progress),
            Msg::KeyInstalled { alias, result } => match result {
                Ok(true) => {
                    self.toast(format!("Key added to {alias}. Next login won't ask for a password."), Tone::Good)
                }
                Ok(false) => self.toast(format!("Your key was already on {alias}."), Tone::Info),
                Err(e) => self.toast(format!("Couldn't copy key to {alias}: {e}"), Tone::Bad),
            },
            Msg::FileOp { session, dir, select, result } => {
                let Some(s) = self.session_by_id(session) else { return };
                let alias = s.alias.clone();
                let refresh = s.files.cwd == dir;
                if refresh {
                    s.files.pending_select = select;
                }
                match result {
                    Ok(text) => self.toast(text, Tone::Good),
                    Err(e) => self.toast(e, Tone::Bad),
                }
                if refresh {
                    self.list_dir(&alias, dir);
                }
            }
            Msg::Preview { session, path, result } => {
                let alias = self.sessions.values().find(|s| s.id == session).map(|s| s.alias.clone());
                if let Some(Popup::Preview(p)) = &mut self.popup
                    && p.path == path
                    && Some(&p.alias) == alias.as_ref()
                {
                    p.body = match result {
                        Ok((bytes, more)) => match preview_lines(&bytes) {
                            Some(lines) => PreviewBody::Text(lines, more),
                            None => PreviewBody::Binary,
                        },
                        Err(e) => PreviewBody::Failed(e),
                    };
                }
            }
            Msg::Forward { session, fwd, cancel, result } => {
                let Some(s) = self.session_by_id(session) else { return };
                let alias = s.alias.clone();
                match result {
                    Ok(()) if cancel => {
                        s.forwards.retain(|f| f != &fwd);
                        self.toast(format!("Stopped forwarding localhost:{}", fwd.port()), Tone::Info);
                    }
                    Ok(()) => {
                        if !s.forwards.contains(&fwd) {
                            s.forwards.push(fwd.clone());
                        }
                        let text = match &fwd {
                            Forward::Socks { port } => format!("SOCKS proxy on localhost:{port} through {alias}"),
                            f => format!("localhost:{} now reaches {} on {alias}", f.port(), f.target()),
                        };
                        self.toast(text, Tone::Good);
                    }
                    Err(e) => {
                        let text = format!("Couldn't forward: {e}");
                        if let Some(Popup::Forwards(p)) = &mut self.popup
                            && p.alias == alias
                        {
                            // Back to the input with what was typed, to fix it.
                            if !cancel {
                                p.input = Some(fwd.spec());
                            }
                            p.error = Some(e);
                        }
                        self.toast(text, Tone::Bad);
                    }
                }
                if let Some(Popup::Forwards(p)) = &mut self.popup {
                    let n = self.sessions.get(&p.alias).map(|s| s.forwards.len()).unwrap_or(0);
                    p.selected = p.selected.min(n.saturating_sub(1));
                }
            }
        }
    }

    fn on_transfer(&mut self, id: u64, progress: Progress) {
        let Some(i) = self.transfers.iter().position(|t| t.id == id) else { return };
        match progress {
            Progress::Update { done, total, file } => {
                let t = &mut self.transfers[i];
                t.done = done;
                if total > 0 {
                    t.total = total;
                }
                if !file.is_empty() {
                    t.file = file;
                }
            }
            Progress::Done(result) => {
                let t = self.transfers.remove(i);
                if matches!(self.popup, Some(Popup::Transfer(p)) if p == id) {
                    self.popup = None;
                }
                match result {
                    Ok(shown) if t.upload => {
                        self.toast(format!("Uploaded {shown}"), Tone::Good);
                        let refresh = self
                            .sessions
                            .get(&t.alias)
                            .filter(|s| s.files.cwd == t.remote_path)
                            .map(|s| s.files.cwd.clone());
                        if let Some(dir) = refresh {
                            self.list_dir(&t.alias, dir);
                        }
                    }
                    Ok(shown) => {
                        self.stamps.insert((t.alias, t.remote_path), t.stamped);
                        self.toast(format!("Saved {shown}"), Tone::Good);
                    }
                    Err(e) if e == "Cancelled" => self.toast(format!("Cancelled {}", t.name), Tone::Info),
                    Err(e) => self.toast(format!("{} failed: {e}", t.name), Tone::Bad),
                }
            }
        }
    }

    /// Periodic housekeeping. Returns true if a redraw is needed.
    pub fn on_tick(&mut self) -> bool {
        let mut changed = false;
        let mut need_sftp = Vec::new();
        for s in self.sessions.values_mut() {
            if s.poll() {
                changed = true;
                match s.state {
                    State::Connected => need_sftp.push(s.alias.clone()),
                    State::Failed(_) | State::Ended(Some(1..)) => {
                        self.attention.insert(s.alias.clone());
                    }
                    _ => {}
                }
            }
        }
        if let Some(cur) = self.current_host().map(|h| h.alias.clone())
            && (self.focus != Focus::Machines || self.sessions.get(&cur).is_some_and(|s| s.is_live()))
        {
            self.attention.remove(&cur);
        }
        for a in need_sftp {
            self.start_sftp(&a);
        }
        if self.toast.as_ref().is_some_and(|t| Instant::now() > t.until) {
            self.toast = None;
            changed = true;
        }
        changed || !self.transfers.is_empty()
    }

    // ---------------------------------------------------------------- input

    /// Handle one terminal event. Returns true if the screen needs a redraw.
    pub fn on_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(text) => self.on_paste(text),
            Event::Mouse(m) => return self.on_mouse(m),
            Event::Resize(..) => {}
            _ => return false,
        }
        true
    }

    // ---------------------------------------------------------------- mouse

    /// The shell pane's inside (without its border).
    fn shell_inner(&self) -> Option<Rect> {
        self.panes.shell.map(|r| Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), r.height.saturating_sub(2)))
    }

    /// Screen position to (row, col) inside the shell. `clamp` pins positions
    /// outside the pane to its nearest edge (for drags).
    fn shell_cell(&self, x: u16, y: u16, clamp: bool) -> Option<(u16, u16)> {
        let r = self.shell_inner().filter(|r| r.width > 0 && r.height > 0)?;
        if clamp {
            let col = x.clamp(r.left(), r.right() - 1) - r.x;
            let row = y.clamp(r.top(), r.bottom() - 1) - r.y;
            return Some((row, col));
        }
        r.contains(Position { x, y }).then(|| (y - r.y, x - r.x))
    }

    fn on_mouse(&mut self, m: MouseEvent) -> bool {
        use MouseEventKind as K;
        if self.onboarding.is_some() {
            return false;
        }
        let (x, y) = (m.column, m.row);
        // A drag that began in the shell follows the pointer anywhere.
        if let Some(grab) = self.grab
            && matches!(m.kind, K::Drag(_) | K::Up(_))
        {
            return self.on_grab(grab, m);
        }
        if self.popup.is_some() {
            return self.on_popup_mouse(m);
        }
        let in_rect = |r: Option<Rect>| r.is_some_and(|r| r.contains(Position { x, y }));
        let wheel = match m.kind {
            K::ScrollUp => Some(-1),
            K::ScrollDown => Some(1),
            _ => None,
        };
        if let Some(dir) = wheel {
            if in_rect(self.panes.shell) {
                self.shell_wheel(dir < 0, m);
            } else if in_rect(Some(self.panes.machines)) {
                self.move_selection(dir);
            } else if in_rect(self.panes.files)
                && let Some(s) = self.current_session_mut()
            {
                let f = &mut s.files;
                f.selected = f.selected.saturating_add_signed(dir).min(f.visible().len().saturating_sub(1));
            }
            return true;
        }
        match m.kind {
            K::Down(button) => {
                // A release lost outside the window must not leave a drag running.
                self.grab = None;
                if let Some(sel) = &mut self.selection {
                    sel.dragging = false;
                }
                if button == MouseButton::Left
                    && let Some(Hit::Key(k)) = self.hit_at(x, y)
                {
                    self.on_key(k);
                    return true;
                }
                if in_rect(self.panes.shell) {
                    self.focus = Focus::Shell;
                    self.shell_press(m);
                    return true;
                }
                if button != MouseButton::Left {
                    return false;
                }
                self.selection = None;
                let count = self.clicks.press(x, y);
                if in_rect(Some(self.panes.machines)) {
                    self.focus = Focus::Machines;
                    self.searching = false;
                    // Hits are from the last frame; the list may have changed since.
                    if let Some(Hit::Machine(i)) = self.hit_at(x, y)
                        && let Some(alias) = self.hosts.get(i).map(|h| h.alias.clone())
                    {
                        self.selected = i;
                        self.attention.remove(&alias);
                        if count == 2 {
                            self.connect_selected();
                        }
                    }
                } else if in_rect(self.panes.files) {
                    self.focus = Focus::Files;
                    if let Some(Hit::File(i)) = self.hit_at(x, y)
                        && let Some(s) = self.current_session_mut()
                    {
                        s.files.filtering = false;
                        s.files.selected = i.min(s.files.visible().len().saturating_sub(1));
                        if count == 2 {
                            self.on_files_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                        }
                    }
                }
                true
            }
            K::Moved => {
                // Hover reports, for programs that asked for every motion.
                if let Some((row, col)) = self.shell_cell(x, y, false)
                    && let Some(s) = self.current_session_mut()
                    && s.is_live()
                    && s.scrollback() == 0
                {
                    let (mode, enc) = s.mouse_mode();
                    if let Some(bytes) = mouse::encode(m.kind, m.modifiers, col, row, mode, enc) {
                        s.write(&bytes);
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn on_popup_mouse(&mut self, m: MouseEvent) -> bool {
        use MouseEventKind as K;
        let (x, y) = (m.column, m.row);
        match m.kind {
            K::ScrollUp | K::ScrollDown => {
                let up = m.kind == K::ScrollUp;
                match &mut self.popup {
                    Some(Popup::Preview(p)) => p.scroll_by(if up { -(mouse::WHEEL_LINES as isize) } else { 3 }),
                    Some(Popup::Help(s)) => *s = if up { s.saturating_sub(2) } else { s.saturating_add(2) },
                    Some(Popup::Forwards(p)) if p.input.is_none() => {
                        let n = self.sessions.get(&p.alias).map(|s| s.forwards.len()).unwrap_or(0);
                        p.selected =
                            if up { p.selected.saturating_sub(1) } else { (p.selected + 1).min(n.saturating_sub(1)) };
                    }
                    _ => return false,
                }
                true
            }
            K::Down(MouseButton::Left) => {
                match self.hit_at(x, y) {
                    Some(Hit::Key(k)) => self.on_key(k),
                    Some(_) => {}
                    // Outside the card: read-only cards close, the rest stay put.
                    None => {
                        if matches!(self.popup, Some(Popup::Help(_) | Popup::Preview(_))) {
                            self.popup = None;
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn shell_press(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        self.selection = None;
        // Clicks on the border only focus.
        let Some((row, col)) = self.shell_cell(x, y, false) else { return };
        let Some(s) = self.current_session_mut() else { return };
        let (mode, enc) = s.mouse_mode();
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        if s.is_live() && mode != vt100::MouseProtocolMode::None && !shift && s.scrollback() == 0 {
            if let Some(bytes) = mouse::encode(m.kind, m.modifiers, col, row, mode, enc) {
                s.write(&bytes);
            }
            self.grab = Some(Grab::Forward);
            return;
        }
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let alias = s.alias.clone();
        let parser = Arc::clone(&s.parser);
        let count = self.clicks.press(x, y);
        let span = match count {
            1 => Some((col, col)),
            2 => parser.lock().ok().and_then(|p| mouse::word_at(p.screen(), row, col)),
            _ => Some((0, self.shell_inner().map(|r| r.width.saturating_sub(1)).unwrap_or(0))),
        };
        let Some((a, b)) = span else { return };
        self.selection = Some(Selection { alias, anchor: (row, a), head: (row, b), dragging: count == 1 });
        if count == 1 {
            self.grab = Some(Grab::Select);
        } else {
            self.copy_selection();
        }
    }

    fn on_grab(&mut self, grab: Grab, m: MouseEvent) -> bool {
        let up = matches!(m.kind, MouseEventKind::Up(_));
        if up {
            self.grab = None;
        }
        let Some((row, col)) = self.shell_cell(m.column, m.row, true) else { return false };
        match grab {
            Grab::Forward => {
                if let Some(s) = self.current_session_mut()
                    && s.is_live()
                {
                    let (mode, enc) = s.mouse_mode();
                    if let Some(bytes) = mouse::encode(m.kind, m.modifiers, col, row, mode, enc) {
                        s.write(&bytes);
                    }
                }
                false
            }
            Grab::Select => {
                let Some(sel) = &mut self.selection else { return false };
                sel.head = (row, col);
                if up {
                    sel.dragging = false;
                    if sel.is_empty() {
                        self.selection = None;
                    } else {
                        self.copy_selection();
                    }
                }
                true
            }
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = &self.selection else { return };
        let Some(s) = self.sessions.get(&sel.alias) else { return };
        let text = match s.parser.lock() {
            Ok(p) => sel.text(p.screen()),
            Err(_) => return,
        };
        if text.trim().is_empty() {
            return;
        }
        clipboard::copy(&text);
        let n = text.chars().count();
        self.toast(format!("Copied {n} character{}", if n == 1 { "" } else { "s" }), Tone::Good);
    }

    /// The wheel over the shell: the remote program gets it if it asked for
    /// the mouse; full-screen programs get arrow keys (like any terminal);
    /// otherwise it scrolls drawer's own scrollback.
    fn shell_wheel(&mut self, up: bool, m: MouseEvent) {
        let cell = self.shell_cell(m.column, m.row, true);
        self.selection = None;
        let Some(s) = self.current_session_mut() else { return };
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        if s.is_live() && s.scrollback() == 0 && !shift {
            let (mode, enc) = s.mouse_mode();
            if mode != vt100::MouseProtocolMode::None {
                let (row, col) = cell.unwrap_or((0, 0));
                if let Some(bytes) = mouse::encode(m.kind, m.modifiers, col, row, mode, enc) {
                    s.write(&bytes);
                }
                return;
            }
            if s.alternate_screen() {
                let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
                if let Some(bytes) = keys::encode(key, s.app_cursor()) {
                    s.write(&bytes.repeat(mouse::WHEEL_LINES));
                }
                return;
            }
        }
        let n = mouse::WHEEL_LINES as isize;
        s.scroll_by(if up { n } else { -n });
    }

    fn on_paste(&mut self, text: String) {
        // Pasting into a text field: one line, no control characters.
        let clean: String = text.lines().next().unwrap_or("").chars().filter(|c| !c.is_control()).collect();
        match &mut self.popup {
            Some(Popup::HostForm(f)) => {
                f.fields[f.focus].push_str(&clean);
                f.error = None;
                return;
            }
            Some(Popup::Upload(p)) => {
                p.input.push_str(&clean);
                p.error = None;
                return;
            }
            Some(Popup::Name(p)) => {
                p.input.push_str(&clean);
                p.error = None;
                return;
            }
            Some(Popup::Forwards(p)) => {
                if let Some(input) = &mut p.input {
                    input.push_str(&clean);
                    p.error = None;
                }
                return;
            }
            Some(_) => return,
            None => {}
        }
        if self.focus == Focus::Files
            && let Some(s) = self.current_session_mut()
            && s.files.filtering
        {
            s.files.filter.push_str(&clean);
            s.files.selected = 0;
            return;
        }
        if self.searching {
            self.filter.push_str(&clean);
            self.snap_selection();
            return;
        }
        if self.focus == Focus::Shell
            && let Some(s) = self.current_session_mut()
        {
            if s.bracketed_paste() {
                let mut v = b"\x1b[200~".to_vec();
                v.extend(text.replace("\x1b[201~", "").as_bytes());
                v.extend(b"\x1b[201~");
                s.write(&v);
            } else {
                s.write(text.as_bytes());
            }
        }
    }

    fn on_onboarding_key(&mut self, k: KeyEvent) {
        let Some(ob) = &mut self.onboarding else { return };
        let last = ob.step + 1 >= onboarding::STEPS;
        match k.code {
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab | KeyCode::Char(' ') if !last => ob.step += 1,
            KeyCode::Enter if !last => ob.step += 1,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => ob.step = ob.step.saturating_sub(1),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Right => {
                onboarding::mark_done();
                self.onboarding = None;
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.onboarding.is_some() {
            return self.on_onboarding_key(k);
        }
        // Typing clears a finished selection, like in any terminal.
        if self.selection.as_ref().is_some_and(|s| !s.dragging) {
            self.selection = None;
        }
        if self.popup.is_some() {
            return self.on_popup_key(k);
        }
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('1') if alt => {
                self.zoom = false;
                return self.focus = Focus::Machines;
            }
            KeyCode::Char('2') if alt => return self.focus = Focus::Shell,
            KeyCode::Char('3') if alt => {
                self.zoom = false;
                return self.focus = Focus::Files;
            }
            KeyCode::Char('z') | KeyCode::Char('Z') if alt => {
                self.zoom = !self.zoom;
                if self.zoom {
                    self.focus = Focus::Shell;
                }
                return;
            }
            KeyCode::Char('q') if alt => return self.request_quit(),
            _ => {}
        }
        match self.focus {
            Focus::Machines => self.on_machines_key(k),
            Focus::Shell => self.on_shell_key(k),
            Focus::Files => self.on_files_key(k),
        }
    }

    fn request_quit(&mut self) {
        let live = self.live_sessions();
        if live > 0 { self.popup = Some(Popup::ConfirmQuit(live)) } else { self.quit = true }
    }

    fn on_common_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Char('?') => self.popup = Some(Popup::Help(0)),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.request_quit(),
            _ => return false,
        }
        true
    }

    fn on_search_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.searching = false;
                self.filter.clear();
            }
            KeyCode::Enter => {
                self.searching = false;
                self.connect_selected();
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => self.filter.clear(),
            KeyCode::Char(c)
                if !c.is_control() && !k.modifiers.contains(KeyModifiers::CONTROL) && self.filter.len() < 64 =>
            {
                self.filter.push(c)
            }
            _ => {}
        }
        self.snap_selection();
    }

    fn on_machines_key(&mut self, k: KeyEvent) {
        if self.searching {
            return self.on_search_key(k);
        }
        if self.on_common_key(k) {
            return;
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.connect_selected(),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Esc if !self.filter.is_empty() => self.filter.clear(),
            KeyCode::Char('a') => self.open_add_form(),
            KeyCode::Char('e') => self.open_edit_form(),
            KeyCode::Char('D') => {
                if let Some(h) = self.current_host() {
                    match h.source.clone() {
                        Some(file) => self.popup = Some(Popup::ConfirmDelete { alias: h.alias.clone(), file }),
                        None => {
                            let msg = format!("{} isn't in a config file", h.alias);
                            self.toast(msg, Tone::Info)
                        }
                    }
                }
            }
            KeyCode::Char('K') => self.ask_copy_key(),
            KeyCode::Char('f') => self.open_forwards(),
            KeyCode::Char('x') => {
                if let Some(h) = self.current_host() {
                    let alias = h.alias.clone();
                    match self.sessions.get(&alias) {
                        Some(s) if s.is_live() => self.popup = Some(Popup::ConfirmClose(alias)),
                        Some(_) => self.close_session(&alias),
                        None => {}
                    }
                }
            }
            KeyCode::Char('r') => {
                self.load_hosts();
                self.toast("Reloaded ssh config", Tone::Info);
            }
            KeyCode::Tab => self.focus = Focus::Shell,
            KeyCode::BackTab => self.focus = Focus::Files,
            _ => {}
        }
        if let Some(a) = self.current_host().map(|h| h.alias.clone()) {
            self.attention.remove(&a);
        }
    }

    fn on_shell_key(&mut self, k: KeyEvent) {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let Some(s) = self.current_session_mut() else {
            // No session: behave like the machines pane so keys are not lost.
            match k.code {
                KeyCode::Enter => self.connect_selected(),
                KeyCode::Tab => self.focus = Focus::Files,
                KeyCode::BackTab => self.focus = Focus::Machines,
                _ => {
                    self.on_common_key(k);
                }
            }
            return;
        };
        let scrolled = s.scrollback() > 0;
        let half = s.half_page();
        let jump = shift || k.modifiers.contains(KeyModifiers::ALT);
        if !s.is_live() {
            match k.code {
                // What happened before the session ended stays readable.
                KeyCode::PageUp => s.scroll_by(half),
                KeyCode::PageDown => s.scroll_by(-half),
                KeyCode::Up => s.scroll_by(1),
                KeyCode::Down => s.scroll_by(-1),
                KeyCode::Esc if scrolled => s.set_scroll(0),
                KeyCode::Enter => self.connect_selected(),
                KeyCode::Char('x') => {
                    let a = s.alias.clone();
                    self.close_session(&a);
                }
                KeyCode::Tab => self.focus = Focus::Files,
                KeyCode::BackTab => self.focus = Focus::Machines,
                _ => {
                    self.on_common_key(k);
                }
            }
            return;
        }
        match k.code {
            // shift+pgup is the classic; many terminals (Ghostty, kitty) keep
            // it for themselves, so alt+pgup does the same.
            KeyCode::PageUp if jump => s.scroll_by(half),
            KeyCode::PageDown if jump => s.scroll_by(-half),
            // While scrolled back, the reading keys scroll instead of typing.
            KeyCode::PageUp if scrolled => s.scroll_by(half),
            KeyCode::PageDown if scrolled => s.scroll_by(-half),
            KeyCode::Up if scrolled => s.scroll_by(1),
            KeyCode::Down if scrolled => s.scroll_by(-1),
            KeyCode::Home if scrolled => s.set_scroll(usize::MAX),
            KeyCode::End | KeyCode::Esc if scrolled => s.set_scroll(0),
            _ => {
                if let Some(bytes) = keys::encode(k, s.app_cursor()) {
                    s.write(&bytes);
                }
            }
        }
    }

    fn on_files_filter_key(&mut self, k: KeyEvent) {
        let Some(s) = self.current_session_mut() else { return };
        let f = &mut s.files;
        let cur = f.current().map(|e| e.name);
        match k.code {
            KeyCode::Esc => {
                f.filtering = false;
                f.filter.clear();
                f.select_name(cur.as_deref());
            }
            KeyCode::Enter => f.filtering = false,
            KeyCode::Up => f.selected = f.selected.saturating_sub(1),
            KeyCode::Down => f.selected = (f.selected + 1).min(f.visible().len().saturating_sub(1)),
            KeyCode::Backspace => {
                if f.filter.pop().is_none() {
                    f.filtering = false;
                }
                f.select_name(cur.as_deref());
            }
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                f.filter.clear();
                f.select_name(cur.as_deref());
            }
            KeyCode::Char(c)
                if !c.is_control() && !k.modifiers.contains(KeyModifiers::CONTROL) && f.filter.len() < 128 =>
            {
                f.filter.push(c);
                f.selected = 0;
            }
            _ => {}
        }
    }

    fn on_files_key(&mut self, k: KeyEvent) {
        if self.current_session().is_some_and(|s| s.files.filtering) {
            return self.on_files_filter_key(k);
        }
        if self.on_common_key(k) {
            return;
        }
        match k.code {
            KeyCode::Tab => return self.focus = Focus::Machines,
            KeyCode::BackTab => return self.focus = Focus::Shell,
            _ => {}
        }
        let Some(alias) = self.current_host().map(|h| h.alias.clone()) else { return };
        let page = self.panes.files.map(|r| r.height.saturating_sub(5) as usize).unwrap_or(10).max(1);
        let Some(s) = self.sessions.get_mut(&alias) else { return };
        if s.files.sftp.is_none() {
            if k.code == KeyCode::Char('r') && s.state == State::Connected {
                self.start_sftp(&alias);
            }
            return;
        }
        enum Act {
            None,
            List(String),
            Download(Entry),
            Upload(String),
            Preview(Entry),
            Name(NameOp),
            Remove(Entry),
            Copy(String),
            Cd(String),
            Note(&'static str),
        }
        let f = &mut s.files;
        let n = f.visible().len();
        let last = n.saturating_sub(1);
        let act = match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                f.selected = f.selected.saturating_sub(1);
                Act::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                f.selected = (f.selected + 1).min(last);
                Act::None
            }
            KeyCode::PageUp => {
                f.selected = f.selected.saturating_sub(page);
                Act::None
            }
            KeyCode::PageDown => {
                f.selected = (f.selected + page).min(last);
                Act::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                f.selected = 0;
                Act::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                f.selected = last;
                Act::None
            }
            KeyCode::Char('.') => {
                let cur = f.current().map(|e| e.name);
                f.show_hidden = !f.show_hidden;
                f.selected = 0;
                f.select_name(cur.as_deref());
                Act::None
            }
            KeyCode::Char('/') => {
                f.filtering = true;
                Act::None
            }
            KeyCode::Esc if !f.filter.is_empty() => {
                let cur = f.current().map(|e| e.name);
                f.filter.clear();
                f.select_name(cur.as_deref());
                Act::None
            }
            KeyCode::Char('s') => {
                let cur = f.current().map(|e| e.name);
                f.sort = f.sort.next();
                f.resort();
                f.select_name(cur.as_deref());
                Act::None
            }
            KeyCode::Char('r') => Act::List(f.cwd.clone()),
            KeyCode::Char('~') => Act::List(f.home.clone()),
            KeyCode::Char('u') => Act::Upload(f.cwd.clone()),
            KeyCode::Char('n') => Act::Name(NameOp::Mkdir),
            KeyCode::Char('c') => Act::Cd(f.cwd.clone()),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace if f.cwd != "/" => {
                f.came_from = f.cwd.trim_end_matches('/').rsplit('/').next().map(str::to_string);
                Act::List(remote::parent(&f.cwd))
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match f.current() {
                Some(e) if e.kind.is_dir() => Act::List(remote::join(&f.cwd, &e.name)),
                Some(e) if matches!(e.kind, Kind::File | Kind::LinkFile) => Act::Preview(e),
                _ => Act::None,
            },
            KeyCode::Char('p') | KeyCode::Char(' ') => match f.current() {
                Some(e) if matches!(e.kind, Kind::File | Kind::LinkFile) => Act::Preview(e),
                Some(e) if e.kind.is_dir() => Act::Note("That's a folder; open it with enter."),
                Some(_) => Act::Note("Only files can be previewed."),
                None => Act::None,
            },
            KeyCode::Char('d') => match f.current() {
                Some(e) if matches!(e.kind, Kind::File | Kind::LinkFile | Kind::Dir) => Act::Download(e),
                Some(e) if e.kind == Kind::LinkDir => {
                    Act::Note("That folder is a link; open it and download from inside.")
                }
                Some(_) => Act::Note("Only files and folders can be downloaded."),
                None => Act::None,
            },
            KeyCode::Char('R') => match f.current() {
                Some(e) => Act::Name(NameOp::Rename(e.name)),
                None => Act::None,
            },
            KeyCode::Char('D') | KeyCode::Delete => f.current().map(Act::Remove).unwrap_or(Act::None),
            KeyCode::Char('y') => match f.current() {
                Some(e) => Act::Copy(remote::join(&f.cwd, &e.name)),
                None => Act::Copy(f.cwd.clone()),
            },
            _ => Act::None,
        };
        let cwd = f.cwd.clone();
        match act {
            Act::None => {}
            Act::List(d) => self.list_dir(&alias, d),
            Act::Download(e) => {
                let path = remote::join(&cwd, &e.name);
                self.ask_download(alias, path, &e.name, e.size, e.kind == Kind::Dir);
            }
            Act::Upload(dir) => {
                let start = download::tilde(&dirs::home_dir().unwrap_or_default()) + "/";
                let mut p = PathInput { alias, remote_dir: dir, input: start, suggestions: Vec::new(), error: None };
                p.suggestions = complete(&p.input).1;
                self.popup = Some(Popup::Upload(p));
            }
            Act::Preview(e) => self.open_preview(&alias, &cwd, &e),
            Act::Name(op) => {
                let input = match &op {
                    NameOp::Rename(old) => old.clone(),
                    NameOp::Mkdir => String::new(),
                };
                self.popup = Some(Popup::Name(NameInput { alias, dir: cwd, op, input, error: None }));
            }
            Act::Remove(e) => {
                self.popup =
                    Some(Popup::ConfirmRemove { alias, dir: cwd, name: e.name.clone(), is_dir: e.kind == Kind::Dir })
            }
            Act::Copy(path) => {
                clipboard::copy(&path);
                self.toast(
                    format!("Copied {}", path.chars().filter(|c| !c.is_control()).collect::<String>()),
                    Tone::Good,
                );
            }
            Act::Cd(dir) => self.cd_shell(&alias, &dir),
            Act::Note(t) => self.toast(t, Tone::Info),
        }
    }

    fn ask_download(&mut self, alias: String, remote_path: String, raw_name: &str, size: u64, is_dir: bool) {
        let name = download::sanitize(raw_name);
        let target = download::preview_target(&self.downloads_dir, &name);
        self.popup = Some(Popup::Download(DownloadAsk { alias, remote_path, name, size, is_dir, target }));
    }

    fn open_preview(&mut self, alias: &str, dir: &str, e: &Entry) {
        let Some(s) = self.sessions.get(alias) else { return };
        let Some(sftp) = s.files.sftp.clone() else { return };
        let path = remote::join(dir, &e.name);
        let (id, tx, p) = (s.id, self.tx.clone(), path.clone());
        tokio::spawn(async move {
            let result = remote::read_head(&sftp, &p, PREVIEW_LIMIT).await;
            let _ = tx.send(Msg::Preview { session: id, path: p, result });
        });
        self.popup = Some(Popup::Preview(Preview {
            alias: alias.to_string(),
            path,
            name: e.name.clone(),
            size: e.size,
            body: PreviewBody::Loading,
            scroll: 0,
            page: Cell::new(10),
        }));
    }

    /// Type `cd '<dir>'` into the shell, without pressing enter: whatever is
    /// running there gets to see it first.
    fn cd_shell(&mut self, alias: &str, dir: &str) {
        let Some(s) = self.sessions.get_mut(alias) else { return };
        if !s.is_live() {
            return self.toast("The shell isn't connected", Tone::Info);
        }
        if s.alternate_screen() {
            return self.toast("Close the full-screen program in the shell first", Tone::Info);
        }
        if dir.chars().any(char::is_control) {
            return self.toast("That folder's name has control characters; cd there by hand", Tone::Info);
        }
        s.write(format!("cd {}", remote::shell_quote(dir)).as_bytes());
        self.focus = Focus::Shell;
        self.toast("Press enter in the shell to go there", Tone::Info);
    }

    fn file_op(&mut self, alias: &str, dir: String, op: NameOp, name: String) {
        let Some(s) = self.sessions.get(alias) else { return };
        let Some(sftp) = s.files.sftp.clone() else {
            return self.toast("The file browser isn't connected", Tone::Bad);
        };
        let (id, tx) = (s.id, self.tx.clone());
        tokio::spawn(async move {
            let target = remote::join(&dir, &name);
            let result = match &op {
                NameOp::Mkdir => remote::make_dir(&sftp, &target).await.map(|_| format!("Created {name}/")),
                NameOp::Rename(old) => {
                    remote::rename(&sftp, &remote::join(&dir, old), &target).await.map(|_| format!("Renamed to {name}"))
                }
            };
            let select = result.is_ok().then_some(name);
            let _ = tx.send(Msg::FileOp { session: id, dir, select, result });
        });
    }

    fn remove_entry(&mut self, alias: &str, dir: String, name: String) {
        let Some(s) = self.sessions.get(alias) else { return };
        let Some(sftp) = s.files.sftp.clone() else {
            return self.toast("The file browser isn't connected", Tone::Bad);
        };
        let (id, tx) = (s.id, self.tx.clone());
        self.toast(format!("Deleting {name}…"), Tone::Info);
        tokio::spawn(async move {
            let result = remote::remove(&sftp, &remote::join(&dir, &name))
                .await
                .map(|n| if n > 1 { format!("Deleted {name} ({n} items)") } else { format!("Deleted {name}") });
            let _ = tx.send(Msg::FileOp { session: id, dir, select: None, result });
        });
    }

    // ------------------------------------------------------------ forwards

    fn open_forwards(&mut self) {
        let Some(h) = self.current_host() else { return };
        let alias = h.alias.clone();
        match self.sessions.get(&alias) {
            Some(s) if s.state == State::Connected => {
                let input = s.forwards.is_empty().then(String::new);
                self.popup = Some(Popup::Forwards(Forwards { alias, selected: 0, input, error: None }));
            }
            _ => self.toast(format!("Connect to {alias} first (enter), then press f"), Tone::Info),
        }
    }

    /// Open a URL in the default browser of the computer drawer runs on.
    fn open_url(&mut self, url: &str) {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let spawned = std::process::Command::new(opener)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.toast(format!("Opening {url}"), Tone::Info);
            }
            Err(_) => self.toast(format!("Couldn't run {opener}; open {url} yourself"), Tone::Bad),
        }
    }

    fn request_forward(&mut self, alias: &str, fwd: Forward, cancel: bool) {
        let Some(s) = self.sessions.get(alias) else { return };
        if s.state != State::Connected {
            return self.toast(format!("{alias} isn't connected"), Tone::Bad);
        }
        let (id, path, tx, alias) = (s.id, s.control_path.clone(), self.tx.clone(), alias.to_string());
        tokio::spawn(async move {
            let result = remote::forward(&alias, &path, &fwd, cancel).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Forward { session: id, fwd, cancel, result });
        });
    }

    fn on_popup_key(&mut self, k: KeyEvent) {
        let Some(popup) = self.popup.take() else { return };
        let yes = matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter);
        let no = matches!(k.code, KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q'));
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match popup {
            Popup::Download(ask) => {
                if yes {
                    self.start_download(ask);
                } else if !no {
                    self.popup = Some(Popup::Download(ask));
                }
            }
            Popup::Transfer(id) => match k.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    if let Some(t) = self.transfers.iter().find(|t| t.id == id) {
                        t.cancel.store(true, Ordering::Relaxed);
                    }
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('h') => {
                    self.toast("Transfer continues in the background", Tone::Info)
                }
                _ => self.popup = Some(Popup::Transfer(id)),
            },
            Popup::Help(scroll) => match k.code {
                KeyCode::Char('w') => {
                    self.onboarding =
                        Some(Onboarding { step: 0, setup: onboarding::Setup::detect(self.downloads_dir.clone()) });
                }
                KeyCode::Down | KeyCode::Char('j') => self.popup = Some(Popup::Help(scroll.saturating_add(1))),
                KeyCode::Up | KeyCode::Char('k') => self.popup = Some(Popup::Help(scroll.saturating_sub(1))),
                _ => {}
            },
            Popup::Preview(mut p) => {
                let page = p.page.get().max(1) as isize;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') | KeyCode::Enter => return,
                    KeyCode::Down | KeyCode::Char('j') => p.scroll_by(1),
                    KeyCode::Up | KeyCode::Char('k') => p.scroll_by(-1),
                    KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('f') => p.scroll_by(page - 1),
                    KeyCode::PageUp | KeyCode::Char('b') => p.scroll_by(1 - page),
                    KeyCode::Home | KeyCode::Char('g') => p.scroll = 0,
                    KeyCode::End | KeyCode::Char('G') => p.scroll = p.max_scroll(),
                    KeyCode::Char('d') => {
                        return self.ask_download(p.alias.clone(), p.path.clone(), &p.name, p.size, false);
                    }
                    KeyCode::Char('y') => {
                        clipboard::copy(&p.path);
                        self.toast(
                            format!("Copied {}", p.path.chars().filter(|c| !c.is_control()).collect::<String>()),
                            Tone::Good,
                        );
                    }
                    _ => {}
                }
                self.popup = Some(Popup::Preview(p));
            }
            Popup::Name(mut p) => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => match remote::check_name(&p.input) {
                        Ok(name) if p.op == NameOp::Rename(name.to_string()) => return,
                        Ok(name) => {
                            let name = name.to_string();
                            return self.file_op(&p.alias, p.dir, p.op, name);
                        }
                        Err(e) => p.error = Some(e.into()),
                    },
                    KeyCode::Backspace => {
                        p.input.pop();
                        p.error = None;
                    }
                    KeyCode::Char('u') if ctrl => p.input.clear(),
                    KeyCode::Char(c) if !ctrl && !c.is_control() && p.input.len() < 255 => {
                        p.input.push(c);
                        p.error = None;
                    }
                    _ => {}
                }
                self.popup = Some(Popup::Name(p));
            }
            Popup::ConfirmRemove { alias, dir, name, is_dir } => {
                // Destructive: only a literal y confirms, never Enter.
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.remove_entry(&alias, dir, name);
                } else if !no {
                    self.popup = Some(Popup::ConfirmRemove { alias, dir, name, is_dir });
                }
            }
            Popup::Forwards(mut p) => {
                let list: Vec<Forward> = self.sessions.get(&p.alias).map(|s| s.forwards.clone()).unwrap_or_default();
                if let Some(input) = &mut p.input {
                    match k.code {
                        KeyCode::Esc if list.is_empty() => return,
                        KeyCode::Esc => {
                            p.input = None;
                            p.error = None;
                        }
                        KeyCode::Enter => match Forward::parse(input) {
                            Ok(fwd) if list.iter().any(|f| f.port() == fwd.port()) => {
                                p.error = Some(format!("localhost:{} is already forwarded", fwd.port()));
                            }
                            Ok(fwd) => {
                                let alias = p.alias.clone();
                                p.input = None;
                                p.error = None;
                                p.selected = list.len();
                                self.popup = Some(Popup::Forwards(p));
                                return self.request_forward(&alias, fwd, false);
                            }
                            Err(e) => p.error = Some(e),
                        },
                        KeyCode::Backspace => {
                            input.pop();
                            p.error = None;
                        }
                        KeyCode::Char('u') if ctrl => input.clear(),
                        KeyCode::Char(c) if !ctrl && !c.is_control() && input.len() < 128 => {
                            input.push(c);
                            p.error = None;
                        }
                        _ => {}
                    }
                } else {
                    let sel = list.get(p.selected).cloned();
                    match k.code {
                        KeyCode::Esc | KeyCode::Char('q') => return,
                        KeyCode::Up | KeyCode::Char('k') => p.selected = p.selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            p.selected = (p.selected + 1).min(list.len().saturating_sub(1))
                        }
                        KeyCode::Char('n') | KeyCode::Char('a') => {
                            p.input = Some(String::new());
                            p.error = None;
                        }
                        KeyCode::Char('x') | KeyCode::Char('D') | KeyCode::Delete => {
                            if let Some(fwd) = sel {
                                let alias = p.alias.clone();
                                self.popup = Some(Popup::Forwards(p));
                                return self.request_forward(&alias, fwd, true);
                            }
                        }
                        KeyCode::Char('o') | KeyCode::Enter => match sel {
                            Some(Forward::Local { port, .. }) => self.open_url(&format!("http://localhost:{port}")),
                            Some(Forward::Socks { .. }) => {
                                self.toast("Point your browser's SOCKS proxy setting at it", Tone::Info)
                            }
                            None => {}
                        },
                        _ => {}
                    }
                }
                self.popup = Some(Popup::Forwards(p));
            }
            Popup::ConfirmQuit(n) => {
                if yes {
                    self.quit = true;
                } else if !no {
                    self.popup = Some(Popup::ConfirmQuit(n));
                }
            }
            Popup::ConfirmClose(alias) => {
                if yes {
                    self.close_session(&alias);
                } else if !no {
                    self.popup = Some(Popup::ConfirmClose(alias));
                }
            }
            Popup::ConfirmDelete { alias, file } => {
                // Destructive: only a literal y confirms, never Enter.
                if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.delete_host(&alias, &file);
                } else if !no {
                    self.popup = Some(Popup::ConfirmDelete { alias, file });
                }
            }
            Popup::CopyKey { alias, key_path, key } => {
                if yes {
                    self.copy_key(alias, key);
                } else if !no {
                    self.popup = Some(Popup::CopyKey { alias, key_path, key });
                }
            }
            Popup::HostForm(mut f) => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Char('s') if ctrl => return self.save_form(f),
                    KeyCode::Enter if f.focus == FORM_LABELS.len() - 1 => return self.save_form(f),
                    KeyCode::Enter | KeyCode::Tab | KeyCode::Down => f.focus = (f.focus + 1) % FORM_LABELS.len(),
                    KeyCode::BackTab | KeyCode::Up => f.focus = (f.focus + FORM_LABELS.len() - 1) % FORM_LABELS.len(),
                    KeyCode::Backspace => {
                        f.fields[f.focus].pop();
                    }
                    KeyCode::Char('u') if ctrl => f.fields[f.focus].clear(),
                    KeyCode::Char(c) if !ctrl && !c.is_control() && f.fields[f.focus].len() < 1024 => {
                        f.fields[f.focus].push(c);
                    }
                    _ => {}
                }
                f.error = None;
                self.popup = Some(Popup::HostForm(f));
            }
            Popup::Upload(mut p) => {
                match k.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => return self.start_upload(p),
                    KeyCode::Tab => {
                        let (completed, sugg) = complete(&p.input);
                        p.input = completed;
                        p.suggestions = sugg;
                    }
                    KeyCode::Backspace => {
                        p.input.pop();
                        p.suggestions = complete(&p.input).1;
                    }
                    KeyCode::Char('u') if ctrl => p.input.clear(),
                    KeyCode::Char(c) if !ctrl && !c.is_control() => {
                        p.input.push(c);
                        p.suggestions = complete(&p.input).1;
                    }
                    _ => {}
                }
                p.error = None;
                self.popup = Some(Popup::Upload(p));
            }
        }
    }

    fn new_transfer(
        &mut self,
        alias: String,
        remote_path: String,
        name: String,
        upload: bool,
        is_dir: bool,
        total: u64,
    ) -> (u64, Arc<AtomicBool>) {
        let id = self.id();
        let cancel = Arc::new(AtomicBool::new(false));
        self.transfers.push(Transfer {
            id,
            alias,
            remote_path,
            file: name.clone(),
            name,
            upload,
            is_dir,
            total,
            done: 0,
            started: Instant::now(),
            cancel: Arc::clone(&cancel),
            stamped: Local::now().date_naive(),
        });
        self.popup = Some(Popup::Transfer(id));
        (id, cancel)
    }

    fn start_download(&mut self, ask: DownloadAsk) {
        let Some(sftp) = self.sessions.get(&ask.alias).and_then(|s| s.files.sftp.clone()) else {
            return self.toast("Connection closed before the download started", Tone::Bad);
        };
        let size = if ask.is_dir { 0 } else { ask.size };
        let (id, cancel) =
            self.new_transfer(ask.alias, ask.remote_path.clone(), ask.name.clone(), false, ask.is_dir, size);
        let tx = self.tx.clone();
        tokio::spawn(transfer::download(
            sftp,
            ask.remote_path,
            ask.is_dir,
            self.downloads_dir.clone(),
            ask.name,
            id,
            cancel,
            move |progress| {
                let _ = tx.send(Msg::Transfer { id, progress });
            },
        ));
    }

    fn start_upload(&mut self, mut p: PathInput) {
        let path = expand_local(&p.input);
        let md = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                p.error = Some("Nothing at that path".into());
                self.popup = Some(Popup::Upload(p));
                return;
            }
        };
        if !(md.is_file() || md.is_dir()) {
            p.error = Some("Pick a regular file or a folder".into());
            self.popup = Some(Popup::Upload(p));
            return;
        }
        let Some(sftp) = self.sessions.get(&p.alias).and_then(|s| s.files.sftp.clone()) else {
            return self.toast("Connection closed before the upload started", Tone::Bad);
        };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let total = if md.is_file() { md.len() } else { 0 };
        let (id, cancel) = self.new_transfer(p.alias, p.remote_dir.clone(), name, true, md.is_dir(), total);
        let tx = self.tx.clone();
        tokio::spawn(transfer::upload(sftp, path, p.remote_dir, id, cancel, move |progress| {
            let _ = tx.send(Msg::Transfer { id, progress });
        }));
    }
}

// --------------------------------------------------------- path completion

fn expand_local(input: &str) -> PathBuf {
    let t = input.trim();
    if t == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = t.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(t)
}

/// Shell-style completion for a local path. Returns the completed input and
/// up to 8 candidate names in the directory being typed into.
pub fn complete(input: &str) -> (String, Vec<String>) {
    let (dir_part, prefix) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    let dir = expand_local(if dir_part.is_empty() { "." } else { dir_part });
    let Ok(rd) = std::fs::read_dir(&dir) else { return (input.to_string(), Vec::new()) };
    let mut names: Vec<(String, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            if !n.starts_with(prefix) || (n.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            let is_dir = e.path().is_dir();
            Some((n, is_dir))
        })
        .collect();
    names.sort();
    let shown: Vec<String> = names.iter().take(8).map(|(n, d)| if *d { format!("{n}/") } else { n.clone() }).collect();
    let completed = match names.as_slice() {
        [] => input.to_string(),
        [(n, d)] => format!("{dir_part}{n}{}", if *d { "/" } else { "" }),
        many => {
            let first = &many[0].0;
            let mut common = first.len();
            for (n, _) in &many[1..] {
                common = common
                    .min(first.chars().zip(n.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum());
            }
            format!("{dir_part}{}", &first[..common])
        }
    };
    (completed, shown)
}

// -------------------------------------------------------------------- loop

pub async fn run(terminal: &mut DefaultTerminal, welcome: bool) -> anyhow::Result<()> {
    let (tx, mut rx) = unbounded_channel();
    let mut app = App::new(tx, welcome);
    app.load_hosts();

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let frame = Duration::from_millis(16);
    let mut last_draw = Instant::now() - frame;
    let mut dirty = true;

    loop {
        if dirty && last_draw.elapsed() >= frame {
            terminal.draw(|f| {
                app.apply_area(f.area());
                ui::draw(f, &app);
            })?;
            last_draw = Instant::now();
            dirty = false;
        }
        if app.quit {
            break;
        }
        let next_frame = tokio::time::Instant::from_std(last_draw + frame);
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(ev)) => { if app.on_event(ev) { dirty = true; } }
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(msg) = rx.recv() => {
                app.on_msg(msg);
                while let Ok(m) = rx.try_recv() { app.on_msg(m); }
                dirty = true;
            }
            _ = tick.tick() => { if app.on_tick() { dirty = true; } }
            _ = tokio::time::sleep_until(next_frame), if dirty => {}
        }
    }
    app.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_text_and_binary() {
        assert_eq!(preview_lines(b"a\tb\r\nline 2\x1b[31m\n").unwrap(), vec!["a   b", "line 2·[31m"]);
        assert_eq!(preview_lines("héllo".as_bytes()).unwrap(), vec!["héllo"]);
        assert!(preview_lines(b"\x7fELF\x02\x01\x00\x00").is_none());
        assert!(preview_lines(b"").unwrap().is_empty());
    }

    #[test]
    fn completes_paths() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().display().to_string();
        std::fs::create_dir(t.path().join("photos")).unwrap();
        std::fs::write(t.path().join("report-a.pdf"), "").unwrap();
        std::fs::write(t.path().join("report-b.pdf"), "").unwrap();
        std::fs::write(t.path().join(".hidden"), "").unwrap();
        assert_eq!(complete(&format!("{d}/ph")).0, format!("{d}/photos/"));
        let (c, s) = complete(&format!("{d}/rep"));
        assert_eq!(c, format!("{d}/report-"));
        assert_eq!(s, vec!["report-a.pdf", "report-b.pdf"]);
        assert!(!complete(&format!("{d}/")).1.contains(&".hidden".to_string()));
        assert!(complete(&format!("{d}/.h")).1.contains(&".hidden".to_string()));
    }
}
