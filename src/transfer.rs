//! Downloads and uploads of files and whole folders over SFTP.
//!
//! Rules shared by both directions:
//! - Never overwrite: the destination gets a fresh name (`name (1)`), claimed
//!   atomically (exclusive create / mkdir / link / server-side rename).
//! - Symlinks inside a folder are skipped, never followed, so a transfer can't
//!   be steered outside the folder you picked.
//! - Every remote name is sanitized before it touches the local disk.
//! - Cancel or failure removes what this transfer created.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::download::{self, numbered};
use crate::remote;

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: usize = 64;

pub enum Progress {
    Update {
        done: u64,
        total: u64,
        file: String,
    },
    /// Ok carries a display path of what was created.
    Done(Result<String, String>),
}

struct Reporter {
    f: Box<dyn Fn(Progress) + Send>,
    last: Instant,
    done: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
}

impl Reporter {
    fn tick(&mut self, n: u64, file: &str, force: bool) -> Result<()> {
        self.done += n;
        if self.cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        if force || self.last.elapsed() >= Duration::from_millis(100) {
            (self.f)(Progress::Update { done: self.done, total: self.total, file: file.to_string() });
            self.last = Instant::now();
        }
        Ok(())
    }
}

fn finish(report: &Reporter, result: Result<String>) {
    (report.f)(Progress::Done(result.map_err(|e| format!("{e:#}"))));
}

fn skipped_note(n: usize) -> String {
    if n == 1 { " (skipped 1 link or special file)".into() } else { format!(" (skipped {n} links or special files)") }
}

// ---------------------------------------------------------------- download

struct RemoteItem {
    remote: String,
    /// Sanitized path components below the root.
    rel: Vec<String>,
    size: u64,
}

/// Walk a remote folder. Directories come back with `size == u64::MAX`.
async fn walk_remote(sftp: &SftpSession, root: &str, skipped: &mut usize) -> Result<Vec<RemoteItem>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_string(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        if rel.len() > MAX_DEPTH {
            bail!("Folder is nested too deeply");
        }
        let entries = sftp.read_dir(dir.clone()).await.with_context(|| format!("cannot read {dir}"))?;
        for e in entries {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let ft = e.metadata().file_type();
            let mut r = rel.clone();
            r.push(download::sanitize(&name));
            let path = remote::join(&dir, &name);
            if ft.is_dir() {
                out.push(RemoteItem { remote: path.clone(), rel: r.clone(), size: u64::MAX });
                stack.push((path, r));
            } else if ft.is_file() {
                out.push(RemoteItem { remote: path, rel: r, size: e.metadata().size.unwrap_or(0) });
            } else {
                *skipped += 1; // symlinks, sockets, devices
            }
            if out.len() > MAX_ENTRIES {
                bail!("Folder has more than {MAX_ENTRIES} entries");
            }
        }
    }
    Ok(out)
}

/// Claim a fresh local directory `dir/name`, `dir/name (1)`, ... atomically.
fn claim_local_dir(dir: &Path, name: &str) -> Result<PathBuf> {
    for n in 0..10_000 {
        let p = dir.join(numbered(name, n));
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", p.display())),
        }
    }
    bail!("too many folders named {name}")
}

async fn copy_remote_to(sftp: &SftpSession, remote: &str, local: &Path, label: &str, rep: &mut Reporter) -> Result<()> {
    let mut src = sftp.open(remote.to_string()).await.with_context(|| format!("cannot open {remote}"))?;
    let mut dst = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(local)
        .await
        .with_context(|| format!("cannot create {}", local.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read(&mut buf).await.context("read failed")?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.context("write failed")?;
        rep.tick(n as u64, label, false)?;
    }
    dst.sync_all().await?;
    Ok(())
}

/// Download a file or a whole folder into `dest_dir`.
#[allow(clippy::too_many_arguments)]
pub async fn download(
    sftp: Arc<SftpSession>,
    remote_path: String,
    is_dir: bool,
    dest_dir: PathBuf,
    name: String,
    id: u64,
    cancel: Arc<AtomicBool>,
    report: impl Fn(Progress) + Send + 'static,
) {
    let mut rep = Reporter { f: Box::new(report), last: Instant::now(), done: 0, total: 0, cancel };
    let result = async {
        tokio::fs::create_dir_all(&dest_dir).await.context("cannot create Downloads folder")?;
        if !is_dir {
            let part = download::part_path(&dest_dir, &name, id);
            let r = async {
                rep.total = sftp.metadata(remote_path.clone()).await.ok().and_then(|m| m.size).unwrap_or(0);
                copy_remote_to(&sftp, &remote_path, &part, &name, &mut rep).await?;
                rep.tick(0, &name, true)?;
                Ok::<_, anyhow::Error>(download::finalize(&part, &dest_dir, &name)?)
            }
            .await;
            if r.is_err() {
                let _ = tokio::fs::remove_file(&part).await;
            }
            return r.map(|p| download::tilde(&p));
        }

        let mut skipped = 0;
        let items = walk_remote(&sftp, &remote_path, &mut skipped).await?;
        rep.total = items.iter().filter(|i| i.size != u64::MAX).map(|i| i.size).sum();
        let root = claim_local_dir(&dest_dir, &name)?;
        let r = async {
            for it in &items {
                let mut local = root.clone();
                for c in &it.rel {
                    local.push(c);
                }
                if it.size == u64::MAX {
                    std::fs::create_dir_all(&local)?;
                    continue;
                }
                if let Some(parent) = local.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Two remote names can sanitize to the same local name.
                let base = local.file_name().unwrap_or_default().to_string_lossy().to_string();
                let parent = local.parent().unwrap_or(&root).to_path_buf();
                let target = download::preview_target(&parent, &base);
                copy_remote_to(&sftp, &it.remote, &target, &it.rel.join("/"), &mut rep).await?;
            }
            rep.tick(0, "", true)?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = r {
            let _ = std::fs::remove_dir_all(&root);
            return Err(e);
        }
        let mut shown = download::tilde(&root);
        if skipped > 0 {
            shown.push_str(&skipped_note(skipped));
        }
        Ok(shown)
    }
    .await;
    finish(&rep, result);
}

// ------------------------------------------------------------------ upload

struct LocalItem {
    local: PathBuf,
    rel: Vec<String>,
    /// None for directories.
    size: Option<u64>,
}

fn walk_local(root: &Path, skipped: &mut usize) -> Result<Vec<LocalItem>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        if rel.len() > MAX_DEPTH {
            bail!("Folder is nested too deeply");
        }
        let mut entries: Vec<_> =
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(name) = e.file_name().into_string() else {
                *skipped += 1; // not valid UTF-8: SFTP paths are strings
                continue;
            };
            let md = std::fs::symlink_metadata(e.path())?;
            let mut r = rel.clone();
            r.push(name);
            if md.is_dir() {
                out.push(LocalItem { local: e.path(), rel: r.clone(), size: None });
                stack.push((e.path(), r));
            } else if md.is_file() {
                out.push(LocalItem { local: e.path(), rel: r, size: Some(md.len()) });
            } else {
                *skipped += 1;
            }
            if out.len() > MAX_ENTRIES {
                bail!("Folder has more than {MAX_ENTRIES} entries");
            }
        }
    }
    Ok(out)
}

async fn copy_local_to(sftp: &SftpSession, local: &Path, remote: &str, label: &str, rep: &mut Reporter) -> Result<()> {
    let mut src = tokio::fs::File::open(local).await.with_context(|| format!("cannot open {}", local.display()))?;
    let mut dst = sftp
        .open_with_flags(remote.to_string(), OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE)
        .await
        .with_context(|| format!("cannot create {remote}"))?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read(&mut buf).await.context("read failed")?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.context("upload failed")?;
        rep.tick(n as u64, label, false)?;
    }
    dst.flush().await.context("upload failed")?;
    dst.close().await.context("upload failed")?;
    Ok(())
}

/// Upload a local file or folder into remote directory `remote_dir`.
pub async fn upload(
    sftp: Arc<SftpSession>,
    local: PathBuf,
    remote_dir: String,
    id: u64,
    cancel: Arc<AtomicBool>,
    report: impl Fn(Progress) + Send + 'static,
) {
    let mut rep = Reporter { f: Box::new(report), last: Instant::now(), done: 0, total: 0, cancel };
    let result = async {
        let md = std::fs::symlink_metadata(&local).with_context(|| format!("{} not found", local.display()))?;
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .context("pick a file or folder, not a path ending in / or ..")?
            .to_string();

        if md.is_file() {
            rep.total = md.len();
            let part = remote::join(&remote_dir, &format!(".{name}.{id}.drawer-part"));
            let r = async {
                copy_local_to(&sftp, &local, &part, &name, &mut rep).await?;
                rep.tick(0, &name, true)?;
                // SFTP rename refuses to replace an existing file: no clobber.
                for n in 0..1000 {
                    let target = remote::join(&remote_dir, &numbered(&name, n));
                    if sftp.try_exists(target.clone()).await.unwrap_or(false) {
                        continue;
                    }
                    if sftp.rename(part.clone(), target.clone()).await.is_ok() {
                        return Ok(target);
                    }
                }
                bail!("could not find a free name for {name}")
            }
            .await;
            if r.is_err() {
                let _ = sftp.remove_file(part.clone()).await;
            }
            return r;
        }
        if !md.is_dir() {
            bail!("Only regular files and folders can be uploaded");
        }

        let mut skipped = 0;
        let items = walk_local(&local, &mut skipped)?;
        rep.total = items.iter().filter_map(|i| i.size).sum();
        // Claim a fresh remote folder: mkdir fails if it already exists.
        let mut root = None;
        for n in 0..1000 {
            let p = remote::join(&remote_dir, &numbered(&name, n));
            if sftp.create_dir(p.clone()).await.is_ok() {
                root = Some(p);
                break;
            }
            if !sftp.try_exists(p).await.unwrap_or(false) {
                bail!("cannot create a folder in {remote_dir} (permission denied?)");
            }
        }
        let root = root.context("could not find a free folder name")?;
        let r = async {
            for it in &items {
                let target = it.rel.iter().fold(root.clone(), |acc, c| remote::join(&acc, c));
                match it.size {
                    None => sftp.create_dir(target.clone()).await.with_context(|| format!("cannot create {target}"))?,
                    Some(_) => copy_local_to(&sftp, &it.local, &target, &it.rel.join("/"), &mut rep).await?,
                }
            }
            rep.tick(0, "", true)?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = r {
            remove_remote_tree(&sftp, &root).await;
            return Err(e);
        }
        let mut shown = root.clone();
        if skipped > 0 {
            shown.push_str(&skipped_note(skipped));
        }
        Ok(shown)
    }
    .await;
    finish(&rep, result);
}

/// Best-effort cleanup of a remote folder this transfer created.
async fn remove_remote_tree(sftp: &SftpSession, root: &str) {
    let mut skipped = 0;
    let Ok(items) = walk_remote(sftp, root, &mut skipped).await else { return };
    let mut dirs = vec![root.to_string()];
    for it in items {
        if it.size == u64::MAX {
            dirs.push(it.remote);
        } else {
            let _ = sftp.remove_file(it.remote).await;
        }
    }
    // Deepest first.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for d in dirs {
        let _ = sftp.remove_dir(d).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_fresh_dirs() {
        let t = tempfile::tempdir().unwrap();
        let a = claim_local_dir(t.path(), "logs").unwrap();
        let b = claim_local_dir(t.path(), "logs").unwrap();
        assert_eq!(a, t.path().join("logs"));
        assert_eq!(b, t.path().join("logs (1)"));
    }

    #[test]
    fn local_walk_skips_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("src");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.txt"), "a").unwrap();
        std::fs::write(d.join("sub/b.txt"), "bb").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", d.join("evil")).unwrap();
        let mut skipped = 0;
        let items = walk_local(&d, &mut skipped).unwrap();
        assert_eq!(skipped, 1);
        let mut rels: Vec<String> = items.iter().map(|i| i.rel.join("/")).collect();
        rels.sort();
        assert_eq!(rels, vec!["a.txt", "sub", "sub/b.txt"]);
        assert_eq!(items.iter().filter_map(|i| i.size).sum::<u64>(), 3);
    }
}
