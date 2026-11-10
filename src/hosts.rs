//! Machines come from the standard OpenSSH config, never a private vault.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Host {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// The config file whose `Host` line defines this alias.
    pub source: Option<PathBuf>,
}

impl Host {
    pub fn new(alias: impl Into<String>) -> Self {
        Self { alias: alias.into(), ..Default::default() }
    }

    /// `user@host:port`, omitting what is unknown or default.
    pub fn address(&self) -> String {
        let host = self.hostname.as_deref().unwrap_or(&self.alias);
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        s.push_str(host);
        if let Some(p) = self.port.filter(|p| *p != 22) {
            s.push(':');
            s.push_str(&p.to_string());
        }
        s
    }
}

pub fn ssh_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(".ssh")
}

/// `DRAWER_CONFIG` points drawer (and every ssh it runs) at another config
/// file, like `ssh -F`. Unset means the normal ~/.ssh/config.
pub fn config_override() -> Option<PathBuf> {
    std::env::var_os("DRAWER_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Arguments every ssh invocation starts with.
pub fn base_args() -> Vec<std::ffi::OsString> {
    match config_override() {
        Some(p) => vec!["-F".into(), p.into_os_string()],
        None => Vec::new(),
    }
}

/// The top-level config file drawer reads.
pub fn main_config() -> PathBuf {
    config_override().unwrap_or_else(|| ssh_dir().join("config"))
}

/// Result of reading the config tree.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    /// Concrete aliases in order, with the file that defines each.
    pub hosts: Vec<(String, PathBuf)>,
    /// Canonical paths of every file read (for "is this file included?").
    pub files: HashSet<PathBuf>,
}

/// Every concrete `Host` alias in the user's config, following `Include`.
pub fn scan() -> Scan {
    let mut scan = Scan::default();
    let mut out = Vec::new();
    parse_file(&main_config(), &ssh_dir(), 0, &mut scan.files, &mut out);
    let mut seen = HashSet::new();
    out.retain(|(a, _)| seen.insert(a.clone()));
    scan.hosts = out;
    scan
}

pub(crate) fn split_kv(line: &str) -> Option<(String, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..idx].to_ascii_lowercase();
    let rest = line[idx..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    Some((key, rest.trim()))
}

/// Split a config value into words, honouring double quotes.
pub(crate) fn words(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in v.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(crate) fn expand(pattern: &str, ssh_dir: &Path) -> PathBuf {
    if let Some(rest) = pattern.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if pattern.starts_with('/') {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    }
}

pub(crate) fn parse_file(
    path: &Path,
    ssh_dir: &Path,
    depth: u8,
    seen_files: &mut HashSet<PathBuf>,
    out: &mut Vec<(String, PathBuf)>,
) {
    if depth > 16 {
        return;
    }
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen_files.insert(canon) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let Some((key, value)) = split_kv(line) else { continue };
        match key.as_str() {
            "host" => {
                for w in words(value) {
                    if !w.contains(['*', '?', '!']) {
                        out.push((w, path.to_path_buf()));
                    }
                }
            }
            "include" => {
                for w in words(value) {
                    let pat = expand(&w, ssh_dir);
                    let Ok(paths) = glob::glob(&pat.to_string_lossy()) else { continue };
                    let mut paths: Vec<_> = paths.flatten().collect();
                    paths.sort();
                    for p in paths {
                        parse_file(&p, ssh_dir, depth + 1, seen_files, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Resolve the effective user/host/port exactly as `ssh` will (`ssh -G`),
/// without connecting.
pub async fn resolve(alias: &str) -> Host {
    let mut host = Host::new(alias);
    let cmd = tokio::process::Command::new("ssh")
        .args(base_args())
        .args(["-G", "--", alias])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(3), cmd).await else {
        return host;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((k, v)) = line.split_once(' ') else { continue };
        match k {
            "hostname" => host.hostname = Some(v.to_string()),
            "user" => host.user = Some(v.to_string()),
            "port" => host.port = v.parse().ok(),
            _ => {}
        }
    }
    host
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_and_includes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("config.d")).unwrap();
        std::fs::write(d.join("config"), "Include config.d/*.conf\n\nHost *\n  ServerAliveInterval 30\n").unwrap();
        std::fs::write(
            d.join("config.d/lab.conf"),
            "# lab\nHost pi nas\n  HostName 10.0.0.2\nHost=\"web box\"\nHost *.internal !bad\nhost router\n",
        )
        .unwrap();
        let mut out = Vec::new();
        parse_file(&d.join("config"), d, 0, &mut HashSet::new(), &mut out);
        let names: Vec<_> = out.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(names, vec!["pi", "nas", "web box", "router"]);
        assert!(out[0].1.ends_with("config.d/lab.conf"));
    }

    #[test]
    fn include_cycles_terminate() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(d.join("config"), "Include config\nHost a\n").unwrap();
        let mut out = Vec::new();
        parse_file(&d.join("config"), d, 0, &mut HashSet::new(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "a");
    }

    #[test]
    fn address_formatting() {
        let h = Host {
            alias: "pi".into(),
            hostname: Some("10.0.0.2".into()),
            user: Some("nv".into()),
            port: Some(2222),
            source: None,
        };
        assert_eq!(h.address(), "nv@10.0.0.2:2222");
        let h = Host { port: Some(22), ..h };
        assert_eq!(h.address(), "nv@10.0.0.2");
        assert_eq!(Host::new("x").address(), "x");
    }
}
