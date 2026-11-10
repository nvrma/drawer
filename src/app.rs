//! Application state and input handling.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use russh_sftp::client::SftpSession;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::configedit::{self, HostSpec};
use crate::download;
use crate::hosts::{self, Host, Scan};
use crate::keys;
use crate::onboarding::{self, Onboarding};
use crate::remote::{self, Entry, Kind};
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
    SftpReady { session: u64, sftp: Arc<SftpSession>, home: String },
    SftpFailed { session: u64, error: String },
    Listing { session: u64, dir: String, result: Result<Vec<Entry>, String> },
    Transfer { id: u64, progress: Progress },
    KeyInstalled { alias: String, result: Result<bool, String> },
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

pub enum Popup {
    Download(DownloadAsk),
    Transfer(u64),
    Help,
    ConfirmQuit(usize),
    ConfirmClose(String),
    HostForm(Form),
    ConfirmDelete { alias: String, file: PathBuf },
    CopyKey { alias: String, key_path: PathBuf, key: String },
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
        self.panes = ui::layout(area, self.focus);
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
                        f.cwd = dir;
                        f.entries = entries;
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

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(text) => self.on_paste(text),
            _ => {}
        }
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
            Some(_) => return,
            None => {}
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
        if self.popup.is_some() {
            return self.on_popup_key(k);
        }
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('1') if alt => return self.focus = Focus::Machines,
            KeyCode::Char('2') if alt => return self.focus = Focus::Shell,
            KeyCode::Char('3') if alt => return self.focus = Focus::Files,
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
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
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
        if !s.is_live() {
            match k.code {
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
            KeyCode::PageUp if shift => {
                let step = (s.size.0 / 2).max(1) as usize;
                s.set_scroll(s.scroll + step);
            }
            KeyCode::PageDown if shift => {
                let step = (s.size.0 / 2).max(1) as usize;
                s.set_scroll(s.scroll.saturating_sub(step));
            }
            _ => {
                if let Some(bytes) = keys::encode(k, s.app_cursor()) {
                    s.write(&bytes);
                }
            }
        }
    }

    fn on_files_key(&mut self, k: KeyEvent) {
        if self.on_common_key(k) {
            return;
        }
        match k.code {
            KeyCode::Tab => return self.focus = Focus::Machines,
            KeyCode::BackTab => return self.focus = Focus::Shell,
            _ => {}
        }
        let Some(alias) = self.current_host().map(|h| h.alias.clone()) else { return };
        let page = self.panes.files.map(|r| r.height.saturating_sub(4) as usize).unwrap_or(10).max(1);
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
            Ask(DownloadAsk),
            Upload(String),
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
                f.selected = cur.and_then(|c| f.visible().iter().position(|e| e.name == c)).unwrap_or(0);
                Act::None
            }
            KeyCode::Char('r') => Act::List(f.cwd.clone()),
            KeyCode::Char('~') => Act::List(f.home.clone()),
            KeyCode::Char('u') => Act::Upload(f.cwd.clone()),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace if f.cwd != "/" => {
                f.came_from = f.cwd.trim_end_matches('/').rsplit('/').next().map(str::to_string);
                Act::List(remote::parent(&f.cwd))
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match f.current() {
                Some(e) if e.kind.is_dir() => Act::List(remote::join(&f.cwd, &e.name)),
                _ => Act::None,
            },
            KeyCode::Char('d') => match f.current() {
                Some(e) if matches!(e.kind, Kind::File | Kind::LinkFile | Kind::Dir) => {
                    let name = download::sanitize(&e.name);
                    let target = download::preview_target(&self.downloads_dir, &name);
                    Act::Ask(DownloadAsk {
                        alias: alias.clone(),
                        remote_path: remote::join(&f.cwd, &e.name),
                        name,
                        size: e.size,
                        is_dir: e.kind == Kind::Dir,
                        target,
                    })
                }
                Some(e) if e.kind == Kind::LinkDir => {
                    Act::Note("That folder is a link; open it and download from inside.")
                }
                Some(_) => Act::Note("Only files and folders can be downloaded."),
                None => Act::None,
            },
            _ => Act::None,
        };
        match act {
            Act::None => {}
            Act::List(d) => self.list_dir(&alias, d),
            Act::Ask(ask) => self.popup = Some(Popup::Download(ask)),
            Act::Upload(dir) => {
                let start = download::tilde(&dirs::home_dir().unwrap_or_default()) + "/";
                let mut p = PathInput { alias, remote_dir: dir, input: start, suggestions: Vec::new(), error: None };
                p.suggestions = complete(&p.input).1;
                self.popup = Some(Popup::Upload(p));
            }
            Act::Note(t) => self.toast(t, Tone::Info),
        }
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
            Popup::Help => {
                if k.code == KeyCode::Char('w') {
                    self.onboarding =
                        Some(Onboarding { step: 0, setup: onboarding::Setup::detect(self.downloads_dir.clone()) });
                }
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
                Some(Ok(ev)) => { app.on_event(ev); dirty = true; }
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
