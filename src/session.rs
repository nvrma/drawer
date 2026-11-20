//! One live SSH session: the system `ssh` client in a PTY, acting as the
//! control master so the file browser can reuse its authenticated connection.
//!
//! Security model: OpenSSH does all host-key verification and authentication.
//! Fingerprint prompts and password prompts appear in the shell pane and go
//! straight from the keyboard to `ssh`; drawer never parses or stores them.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use russh_sftp::client::SftpSession;

use crate::remote::{Entry, Forward};

pub const SCROLLBACK: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Waiting on host key / password / network.
    Connecting,
    Connected,
    /// The shell exited after a successful login.
    Ended(Option<u32>),
    /// ssh gave up before logging in (refused, bad password, host key mismatch).
    Failed(Option<u32>),
}

/// How the file list is ordered. Folders always come first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    /// Newest first.
    Modified,
    /// Largest first.
    Size,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Sort::Name => Sort::Modified,
            Sort::Modified => Sort::Size,
            Sort::Size => Sort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Modified => "newest",
            Sort::Size => "largest",
        }
    }
}

#[derive(Default)]
pub struct Files {
    pub sftp: Option<Arc<SftpSession>>,
    pub home: String,
    pub cwd: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub show_hidden: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Remember the selected name per directory so `h` lands where you were.
    pub came_from: Option<String>,
    /// Name to select once the next listing arrives (after rename, new folder).
    pub pending_select: Option<String>,
    /// Narrow the list to names containing this (case-insensitive).
    pub filter: String,
    /// Typing goes into `filter`.
    pub filtering: bool,
    pub sort: Sort,
}

impl Files {
    pub fn visible(&self) -> Vec<&Entry> {
        let q = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| self.show_hidden || !e.name.starts_with('.'))
            .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
            .collect()
    }
    pub fn hidden_count(&self) -> usize {
        if self.show_hidden { 0 } else { self.entries.iter().filter(|e| e.name.starts_with('.')).count() }
    }
    pub fn current(&self) -> Option<Entry> {
        self.visible().get(self.selected).map(|e| (*e).clone())
    }

    /// Re-order `entries` for the current sort, folders first.
    pub fn resort(&mut self) {
        let by_name =
            |a: &Entry, b: &Entry| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.name.cmp(&b.name));
        let sort = self.sort;
        self.entries.sort_by(|a, b| {
            b.kind.is_dir().cmp(&a.kind.is_dir()).then_with(|| match sort {
                Sort::Name => by_name(a, b),
                Sort::Modified => b.mtime.cmp(&a.mtime).then_with(|| by_name(a, b)),
                Sort::Size => b.size.cmp(&a.size).then_with(|| by_name(a, b)),
            })
        });
    }

    /// Select the entry called `name` if it is visible, else keep the index in range.
    pub fn select_name(&mut self, name: Option<&str>) {
        let vis = self.visible();
        self.selected = name
            .and_then(|n| vis.iter().position(|e| e.name == n))
            .unwrap_or(self.selected)
            .min(vis.len().saturating_sub(1));
    }
}

pub struct Session {
    pub id: u64,
    pub alias: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    pub control_path: PathBuf,
    pub state: State,
    pub size: (u16, u16),
    pub files: Files,
    pub sftp_starting: bool,
    /// Port forwards added through this session's control connection.
    pub forwards: Vec<Forward>,
}

/// Private directory for control sockets: $XDG_RUNTIME_DIR/drawer, or
/// /tmp/drawer-<uid>. Refuses to use a directory that is a symlink, not ours,
/// or readable by anyone else.
pub fn runtime_dir() -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(base) if base.is_dir() => base.join("drawer"),
        _ => std::env::temp_dir().join(format!("drawer-{uid}")),
    };
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).context("cannot create socket directory"),
    }
    let md = std::fs::symlink_metadata(&dir)?;
    if !md.is_dir() || md.uid() != uid {
        bail!("{} is not a directory owned by you", dir.display());
    }
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

impl Session {
    pub fn spawn(
        id: u64,
        alias: &str,
        rows: u16,
        cols: u16,
        socket_dir: &Path,
        on_output: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let rows = rows.max(2);
        let cols = cols.max(10);
        let control_path = socket_dir.join(format!("{}-{id}.sock", std::process::id()));
        let _ = std::fs::remove_file(&control_path);

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("cannot open a pseudo-terminal")?;

        let mut cmd = CommandBuilder::new("ssh");
        cmd.args(crate::hosts::base_args());
        cmd.args(["-o", "ControlMaster=yes", "-o"]);
        cmd.arg(format!("ControlPath={}", control_path.display()));
        cmd.args(["-o", "ControlPersist=no", "--", alias]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd).context("cannot start ssh (is OpenSSH installed?)")?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let p = Arc::clone(&parser);
        std::thread::Builder::new().name(format!("pty-{id}")).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut parser) = p.lock() {
                            parser.process(&buf[..n]);
                        }
                        on_output();
                    }
                }
            }
            on_output();
        })?;

        Ok(Self {
            id,
            alias: alias.to_string(),
            parser,
            writer,
            master: pair.master,
            child,
            control_path,
            state: State::Connecting,
            size: (rows, cols),
            files: Files::default(),
            sftp_starting: false,
            forwards: Vec::new(),
        })
    }

    pub fn is_live(&self) -> bool {
        matches!(self.state, State::Connecting | State::Connected)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if !self.is_live() {
            return;
        }
        if self.scrollback() != 0 {
            self.set_scroll(0);
        }
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// How many lines the view is scrolled back (0 = live). Read from the
    /// emulator each time: new output while scrolled back moves the offset so
    /// the text you're reading stays put.
    pub fn scrollback(&self) -> usize {
        self.parser.lock().map(|p| p.screen().scrollback()).unwrap_or(0)
    }

    pub fn set_scroll(&mut self, lines: usize) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(lines);
        }
    }

    /// Scroll back (positive) or toward live (negative), clamped.
    pub fn scroll_by(&mut self, delta: isize) {
        let cur = self.scrollback();
        self.set_scroll(cur.saturating_add_signed(delta));
    }

    pub fn half_page(&self) -> isize {
        (self.size.0 / 2).max(1) as isize
    }

    /// A full-screen program (vim, htop, less) is showing.
    pub fn alternate_screen(&self) -> bool {
        self.parser.lock().map(|p| p.screen().alternate_screen()).unwrap_or(false)
    }

    /// The mouse mode and encoding the remote program asked for.
    pub fn mouse_mode(&self) -> (vt100::MouseProtocolMode, vt100::MouseProtocolEncoding) {
        self.parser
            .lock()
            .map(|p| (p.screen().mouse_protocol_mode(), p.screen().mouse_protocol_encoding()))
            .unwrap_or_default()
    }

    pub fn app_cursor(&self) -> bool {
        self.parser.lock().map(|p| p.screen().application_cursor()).unwrap_or(false)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.lock().map(|p| p.screen().bracketed_paste()).unwrap_or(false)
    }

    /// Advance the state machine. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        if !self.is_live() {
            return false;
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            let code = Some(status.exit_code());
            self.state = if self.state == State::Connected { State::Ended(code) } else { State::Failed(code) };
            self.files.sftp = None;
            // Forwards live in the master process, so they ended with it.
            self.forwards.clear();
            return true;
        }
        // OpenSSH only opens the control socket after authentication succeeds.
        if self.state == State::Connecting && self.control_path.exists() {
            self.state = State::Connected;
            return true;
        }
        false
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.try_wait();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Kind;

    fn e(name: &str, kind: Kind, size: u64, mtime: i64) -> Entry {
        Entry { name: name.into(), kind, size, mtime: Some(mtime) }
    }

    #[test]
    fn sort_and_filter_keep_folders_first() {
        let mut f = Files {
            entries: vec![
                e("b.log", Kind::File, 10, 300),
                e("src", Kind::Dir, 0, 100),
                e("A.txt", Kind::File, 500, 200),
                e(".env", Kind::File, 1, 400),
            ],
            ..Default::default()
        };
        f.resort();
        let names = |f: &Files| f.visible().iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&f), ["src", "A.txt", "b.log"]);
        f.sort = Sort::Modified;
        f.resort();
        assert_eq!(names(&f), ["src", "b.log", "A.txt"]);
        f.sort = Sort::Size;
        f.resort();
        assert_eq!(names(&f), ["src", "A.txt", "b.log"]);
        f.filter = "LOG".into();
        assert_eq!(names(&f), ["b.log"]);
        f.filter.clear();
        f.show_hidden = true;
        assert_eq!(f.visible().len(), 4);
        f.selected = 3;
        f.select_name(Some("src"));
        assert_eq!(f.selected, 0);
        f.filter = "zzz".into();
        f.select_name(None);
        assert_eq!(f.selected, 0);
    }
}
