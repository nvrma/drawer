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

// ------------------------------------------------------------ file actions

/// A new name typed for a remote file or folder: one path component, no
/// control characters, not `.`/`..`.
pub fn check_name(name: &str) -> Result<&str, &'static str> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Type a name");
    }
    if n == "." || n == ".." {
        return Err("That name is reserved");
    }
    if n.contains('/') {
        return Err("Names can't contain /");
    }
    if n.chars().any(char::is_control) {
        return Err("Names can't contain control characters");
    }
    if n.len() > 255 {
        return Err("That name is too long");
    }
    Ok(n)
}

/// Create a folder. Fails if anything already has that name.
pub async fn make_dir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    if sftp.try_exists(path.to_string()).await.unwrap_or(false) {
        return Err("Something with that name already exists".into());
    }
    sftp.create_dir(path.to_string()).await.map_err(|e| e.to_string())
}

/// Rename within the same folder, never replacing an existing entry.
pub async fn rename(sftp: &SftpSession, from: &str, to: &str) -> Result<(), String> {
    if sftp.symlink_metadata(to.to_string()).await.is_ok() {
        return Err("Something with that name already exists".into());
    }
    // Plain SSH_FXP_RENAME, which OpenSSH refuses when the target exists.
    sftp.rename(from.to_string(), to.to_string()).await.map_err(|e| e.to_string())
}

/// Delete a file, link or folder. Folders are removed with everything in
/// them; links inside are removed as links, never followed. Returns how many
/// entries were removed.
pub async fn remove(sftp: &SftpSession, path: &str) -> Result<usize, String> {
    const MAX: usize = 200_000;
    let md = sftp.symlink_metadata(path.to_string()).await.map_err(|e| e.to_string())?;
    if !md.file_type().is_dir() {
        sftp.remove_file(path.to_string()).await.map_err(|e| e.to_string())?;
        return Ok(1);
    }
    // Depth-first: list every folder, delete files as we go, then the
    // folders deepest first.
    let mut dirs = vec![path.to_string()];
    let mut stack = vec![path.to_string()];
    let mut removed = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = sftp.read_dir(dir.clone()).await.map_err(|e| format!("cannot read {dir}: {e}"))?;
        for e in entries {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = join(&dir, &name);
            if e.metadata().file_type().is_dir() {
                dirs.push(child.clone());
                stack.push(child);
            } else {
                sftp.remove_file(child.clone()).await.map_err(|e| format!("cannot delete {child}: {e}"))?;
                removed += 1;
            }
            if removed + dirs.len() > MAX {
                return Err(format!("More than {MAX} entries; delete this from the shell instead"));
            }
        }
    }
    for d in dirs.iter().rev() {
        sftp.remove_dir(d.clone()).await.map_err(|e| format!("cannot delete {d}: {e}"))?;
        removed += 1;
    }
    Ok(removed)
}

/// Read the start of a file for preview. Returns the bytes and whether the
/// file goes on past `limit`.
pub async fn read_head(sftp: &SftpSession, path: &str, limit: usize) -> Result<(Vec<u8>, bool), String> {
    use tokio::io::AsyncReadExt;
    let file = sftp.open(path.to_string()).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
    let more = buf.len() > limit;
    buf.truncate(limit);
    Ok((buf, more))
}

/// Quote a path for a POSIX shell (and fish): `'it'\''s'`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------- port forwards

/// A port forward added to a live session's control connection. Local ports
/// are always bound to 127.0.0.1, never to the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forward {
    /// localhost:`port` here reaches `host`:`host_port` as seen from the machine.
    Local { port: u16, host: String, host_port: u16 },
    /// A SOCKS proxy on localhost:`port` that sends traffic out via the machine.
    Socks { port: u16 },
}

fn parse_port(s: &str) -> Result<u16, String> {
    match s.trim().parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(format!("'{}' isn't a port (1-65535)", s.trim())),
    }
}

fn check_host(h: &str) -> Result<String, String> {
    let ok = !h.is_empty()
        && h.len() <= 253
        && !h.starts_with('-')
        && h.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c));
    let ipv6 = h.len() > 2
        && h.starts_with('[')
        && h.ends_with(']')
        && h[1..h.len() - 1].chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    if ok || ipv6 { Ok(h.to_string()) } else { Err(format!("'{h}' isn't a host name or address")) }
}

impl Forward {
    /// Accepts `3000`, `8080:3000`, `8080:host:3000` (ssh -L order),
    /// `8080 -> host:3000`, and `socks` / `socks 1080` for a SOCKS proxy.
    pub fn parse(input: &str) -> Result<Self, String> {
        let s = input.trim().replace("->", " ").replace('→', " ");
        let lower = s.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("socks") {
            let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ':' || c == '5');
            let port = if rest.trim().is_empty() { 1080 } else { parse_port(rest)? };
            return Ok(Forward::Socks { port });
        }
        let s = s.split_whitespace().collect::<Vec<_>>().join(":");
        if s.is_empty() {
            return Err("Type a port, like 3000".into());
        }
        let (local, rest) = match s.split_once(':') {
            Some((l, r)) => (l, Some(r)),
            None => (s.as_str(), None),
        };
        let port = parse_port(local)?;
        let (host, host_port) = match rest {
            None => ("localhost".to_string(), port),
            Some(r) if r.chars().all(|c| c.is_ascii_digit()) => ("localhost".to_string(), parse_port(r)?),
            Some(r) => match r.rsplit_once(':') {
                Some((h, p)) => (check_host(h)?, parse_port(p)?),
                None => return Err("Say which port on that host, like host:80".into()),
            },
        };
        Ok(Forward::Local { port, host, host_port })
    }

    pub fn port(&self) -> u16 {
        match self {
            Forward::Local { port, .. } | Forward::Socks { port } => *port,
        }
    }

    /// The `ssh -O forward` option pair.
    fn args(&self) -> [String; 2] {
        match self {
            Forward::Local { port, host, host_port } => ["-L".into(), format!("127.0.0.1:{port}:{host}:{host_port}")],
            Forward::Socks { port } => ["-D".into(), format!("127.0.0.1:{port}")],
        }
    }

    /// Written back the way it can be typed in.
    pub fn spec(&self) -> String {
        match self {
            Forward::Local { port, host, host_port } if host == "localhost" && port == host_port => port.to_string(),
            Forward::Local { port, host, host_port } if host == "localhost" => format!("{port}:{host_port}"),
            Forward::Local { port, host, host_port } => format!("{port}:{host}:{host_port}"),
            Forward::Socks { port } => format!("socks {port}"),
        }
    }

    /// Where it goes, as seen from the machine: `localhost:3000`, or `SOCKS proxy`.
    pub fn target(&self) -> String {
        match self {
            Forward::Local { host, host_port, .. } => format!("{host}:{host_port}"),
            Forward::Socks { .. } => "SOCKS proxy".into(),
        }
    }
}

/// Whether 127.0.0.1:`port` can be listened on, in words if not.
fn port_free(port: u16) -> Result<()> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            anyhow::bail!("ports below 1024 need root; pick a higher one")
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!("localhost:{port} is already in use on this computer")
        }
        Err(e) => anyhow::bail!("can't listen on localhost:{port}: {e}"),
    }
}

/// Add (`cancel == false`) or remove a forward on the session's existing
/// connection with `ssh -O forward|cancel`. No new login.
pub async fn forward(alias: &str, control_path: &Path, fwd: &Forward, cancel: bool) -> Result<()> {
    // Check the local port first: if ssh's own bind fails, the master prints
    // the error into the shell pane.
    if !cancel {
        port_free(fwd.port())?;
    }
    let [flag, spec] = fwd.args();
    // `-F /dev/null`: a mux request also sends every LocalForward from the
    // config, so `cancel` would tear those down too. Only ControlPath matters.
    let out = tokio::process::Command::new("ssh")
        .args(["-F", "/dev/null"])
        .arg("-o")
        .arg(format!("ControlPath={}", control_path.display()))
        .args(["-o", "BatchMode=yes", "-O", if cancel { "cancel" } else { "forward" }])
        .args([flag, spec])
        .args(["--", alias])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), out).await.context("timed out")??;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let line = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("ssh refused").trim();
    let msg = line.rsplit(": ").next().unwrap_or(line);
    // "mux_client_forward: master forward request failed" (or older wording)
    // The master binds the local port itself, so that bind is what failed.
    if (msg.contains("forward") && msg.contains("failed")) || msg.contains("in use") {
        if fwd.port() < 1024 {
            anyhow::bail!("ports below 1024 need root; pick a higher one");
        }
        anyhow::bail!("localhost:{} is already in use on this computer", fwd.port());
    }
    anyhow::bail!("{msg}")
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
    fn forward_specs() {
        let local = |port, host: &str, host_port| Forward::Local { port, host: host.into(), host_port };
        assert_eq!(Forward::parse("3000"), Ok(local(3000, "localhost", 3000)));
        assert_eq!(Forward::parse(" 8080:3000 "), Ok(local(8080, "localhost", 3000)));
        assert_eq!(Forward::parse("8080:10.0.0.5:80"), Ok(local(8080, "10.0.0.5", 80)));
        assert_eq!(Forward::parse("8080 -> db.lan:5432"), Ok(local(8080, "db.lan", 5432)));
        assert_eq!(Forward::parse("8080 → [::1]:80"), Ok(local(8080, "[::1]", 80)));
        assert_eq!(Forward::parse("socks"), Ok(Forward::Socks { port: 1080 }));
        assert_eq!(Forward::parse("SOCKS 9050"), Ok(Forward::Socks { port: 9050 }));
        assert_eq!(Forward::parse("socks5:1081"), Ok(Forward::Socks { port: 1081 }));
        assert!(Forward::parse("").is_err());
        assert!(Forward::parse("0").is_err());
        assert!(Forward::parse("70000").is_err());
        assert!(Forward::parse("8080:host").is_err());
        assert!(Forward::parse("8080:-oProxyCommand=x:80").is_err());
        assert!(Forward::parse("8080:a b:80").is_err());
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(port_free(busy.local_addr().unwrap().port()).is_err());
        assert_eq!(local(8080, "localhost", 3000).args()[1], "127.0.0.1:8080:localhost:3000");
        for spec in ["3000", "8080:3000", "8080:10.0.0.5:80", "socks 1080"] {
            assert_eq!(Forward::parse(spec).unwrap().spec(), spec);
        }
        assert_eq!(Forward::Socks { port: 1080 }.args(), ["-D".to_string(), "127.0.0.1:1080".to_string()]);
    }

    #[test]
    fn names_and_quoting() {
        assert_eq!(check_name("  notes.txt "), Ok("notes.txt"));
        assert!(check_name("a/b").is_err());
        assert!(check_name("..").is_err());
        assert!(check_name("").is_err());
        assert!(check_name("a\nb").is_err());
        assert_eq!(shell_quote("/srv/it's here"), r"'/srv/it'\''s here'");
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
