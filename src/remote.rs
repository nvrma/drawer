//! Remote file browsing and downloads over SFTP, multiplexed through the
//! already-authenticated OpenSSH control connection (no second login).

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use russh_sftp::client::SftpSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    LinkDir,
    LinkFile,
    Other,
}

impl Kind {
    pub fn is_dir(self) -> bool {
        matches!(self, Kind::Dir | Kind::LinkDir)
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: Option<i64>,
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// Open an SFTP subsystem through the control socket. `BatchMode` guarantees
/// this never prompts: if the master is gone it fails instead of asking.
pub async fn connect(alias: &str, control_path: &Path) -> Result<(Arc<SftpSession>, String)> {
    let mut child = tokio::process::Command::new("ssh")
        .args(crate::hosts::base_args())
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg(format!("ControlPath={}", control_path.display()))
        .args(["-o", "BatchMode=yes", "-T", "-s", "--", alias, "sftp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("could not start ssh for the file browser")?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    // Keep the child alive (and reaped) for as long as the stream is in use.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    let stream = tokio::io::join(stdout, stdin);
    let sftp = tokio::time::timeout(Duration::from_secs(15), SftpSession::new(stream))
        .await
        .context("file browser timed out")?
        .context("server refused the SFTP subsystem")?;
    let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".to_string());
    Ok((Arc::new(sftp), home))
}

pub async fn list(sftp: &SftpSession, dir: &str) -> Result<Vec<Entry>, String> {
    let rd = sftp.read_dir(dir).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for e in rd {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let md = e.metadata();
        let ft = md.file_type();
        let mut entry = Entry {
            kind: if ft.is_dir() {
                Kind::Dir
            } else if ft.is_file() {
                Kind::File
            } else if ft.is_symlink() {
                Kind::LinkFile
            } else {
                Kind::Other
            },
            size: md.size.unwrap_or(0),
            mtime: md.mtime.map(i64::from),
            name,
        };
        if entry.kind == Kind::LinkFile {
            match sftp.metadata(join(dir, &entry.name)).await {
                Ok(t) if t.file_type().is_dir() => entry.kind = Kind::LinkDir,
                Ok(t) => entry.size = t.size.unwrap_or(0),
                Err(_) => entry.kind = Kind::Other, // dangling link
            }
        }
        out.push(entry);
    }
    out.sort_by(|a, b| {
        b.kind
            .is_dir()
            .cmp(&a.kind.is_dir())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)))
    });
    Ok(out)
}

/// Remote script for adding a public key. It is a fixed string: the key
/// arrives on stdin, so nothing user-controlled is ever interpolated into
/// shell code. Prints `added` or `present`.
const INSTALL_KEY_SCRIPT: &str = r#"umask 077; d="$HOME/.ssh"; f="$d/authorized_keys"; mkdir -p "$d" && touch "$f" && IFS= read -r k && if grep -qxF -- "$k" "$f"; then echo present; else if [ -s "$f" ] && [ -n "$(tail -c1 "$f")" ]; then echo >> "$f"; fi; printf '%s\n' "$k" >> "$f" && echo added; fi"#;

/// A public key line safe to send: one line, known type, printable.
pub fn valid_pubkey(line: &str) -> bool {
    let l = line.trim();
    let known = ["ssh-ed25519 ", "ssh-rsa ", "ecdsa-sha2-", "sk-ssh-ed25519@openssh.com ", "sk-ecdsa-sha2-"];
    l.len() < 16 * 1024 && known.iter().any(|k| l.starts_with(k)) && !l.chars().any(|c| c.is_control())
}

/// Append `pubkey` to the remote authorized_keys over the existing control
/// connection (no new login). Returns true if added, false if already there.
pub async fn install_key(alias: &str, control_path: &Path, pubkey: &str) -> Result<bool> {
    if !valid_pubkey(pubkey) {
        anyhow::bail!("that doesn't look like an SSH public key");
    }
    let mut child = tokio::process::Command::new("ssh")
        .args(crate::hosts::base_args())
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg(format!("ControlPath={}", control_path.display()))
        .args(["-o", "BatchMode=yes", "-T", "--", alias, INSTALL_KEY_SCRIPT])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start ssh")?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(format!("{}\n", pubkey.trim()).as_bytes()).await?;
    }
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output()).await.context("timed out")??;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match stdout.trim() {
        "added" => Ok(true),
        "present" => Ok(false),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("{}", err.lines().last().unwrap_or("remote command failed").trim())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/home/nv", "x"), "/home/nv/x");
        assert_eq!(parent("/home/nv"), "/home");
        assert_eq!(parent("/home"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(parent("/a/b/"), "/a");
    }

    #[test]
    fn pubkey_validation() {
        assert!(valid_pubkey("ssh-ed25519 AAAAC3Nza nv@omarchy"));
        assert!(!valid_pubkey("ssh-ed25519 AAA\nrm -rf ~"));
        assert!(!valid_pubkey(
            "ssh-ed25519 AAA
evil"
        ));
        assert!(!valid_pubkey("-----BEGIN OPENSSH PRIVATE KEY-----"));
    }
}
