//! Safe edits to OpenSSH config files: add, change and remove `Host` blocks.
//!
//! Safety rules:
//! - Every value is validated against a strict character set before it is
//!   written. A newline or stray directive in a field could otherwise inject
//!   options such as `ProxyCommand`, which runs arbitrary commands.
//! - Symlinks (dotfiles) are followed, and the real file is replaced
//!   atomically: write a temp file beside it, fsync, rename over.
//! - The previous version is kept as `<file>.drawer.bak`.
//! - If the file changed on disk since it was read, nothing is written.
//! - Permissions of the original file are preserved; new files are 0600.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::hosts::{split_kv, words};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSpec {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    /// Empty means "default" (22, or whatever `Host *` says).
    pub port: String,
    /// Empty means "use ssh's defaults / agent".
    pub identity_file: String,
}

// ------------------------------------------------------------- validation

fn check(field: &str, v: &str, max: usize, ok: impl Fn(char) -> bool) -> Result<()> {
    if v.len() > max {
        bail!("{field} is too long (max {max} characters)");
    }
    if v.starts_with('-') {
        bail!("{field} can't start with '-'");
    }
    if let Some(c) = v.chars().find(|c| !ok(*c)) {
        if c.is_whitespace() {
            bail!("{field} can't contain spaces");
        }
        bail!("{field} can't contain '{}'", c.escape_default());
    }
    Ok(())
}

impl HostSpec {
    /// Trim fields and check every one. Returns the cleaned spec.
    pub fn validated(&self) -> Result<HostSpec> {
        let s = HostSpec {
            alias: self.alias.trim().to_string(),
            hostname: self.hostname.trim().to_string(),
            user: self.user.trim().to_string(),
            port: self.port.trim().to_string(),
            identity_file: self.identity_file.trim().to_string(),
        };
        if s.alias.is_empty() {
            bail!("Name is required");
        }
        check("Name", &s.alias, 64, |c| c.is_ascii_alphanumeric() || "._-".contains(c))?;
        if s.hostname.is_empty() {
            bail!("Address is required");
        }
        check("Address", &s.hostname, 253, |c| c.is_ascii_alphanumeric() || ".-_:[]%".contains(c))?;
        check("User", &s.user, 32, |c| c.is_ascii_alphanumeric() || "._-".contains(c))?;
        if !s.port.is_empty() {
            match s.port.parse::<u32>() {
                Ok(p) if (1..=65535).contains(&p) => {}
                _ => bail!("Port must be a number from 1 to 65535"),
            }
        }
        if !s.identity_file.is_empty() {
            check("Key file", &s.identity_file, 1024, |c| {
                !c.is_control() && !c.is_whitespace() && !"\"'\\#=".contains(c) && c != '%'
            })?;
            let p = crate::hosts::expand(&s.identity_file, &crate::hosts::ssh_dir());
            if !p.is_file() {
                bail!("Key file not found: {}", s.identity_file);
            }
        }
        Ok(s)
    }

    fn directives(&self) -> Vec<(&'static str, &str)> {
        let mut v = vec![("HostName", self.hostname.as_str())];
        if !self.user.is_empty() {
            v.push(("User", &self.user));
        }
        if !self.port.is_empty() {
            v.push(("Port", &self.port));
        }
        if !self.identity_file.is_empty() {
            v.push(("IdentityFile", &self.identity_file));
        }
        v
    }
}

// ---------------------------------------------------------------- parsing

fn is_section(line: &str) -> bool {
    matches!(split_kv(line), Some((k, _)) if k == "host" || k == "match")
}

fn host_patterns(line: &str) -> Option<Vec<String>> {
    match split_kv(line) {
        Some((k, v)) if k == "host" => Some(words(v)),
        _ => None,
    }
}

/// Line range `[start, end)` of the block whose `Host` line names `alias`.
fn find_block(lines: &[String], alias: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| host_patterns(l).is_some_and(|p| p.iter().any(|w| w == alias)))?;
    let mut end = lines[start + 1..].iter().position(|l| is_section(l)).map_or(lines.len(), |i| start + 1 + i);
    // Comments directly above the next section describe that section.
    while end > start + 1 && lines[end - 1].trim_start().starts_with('#') {
        end -= 1;
    }
    Some((start, end))
}

/// The current values of a host's block, for pre-filling the edit form.
pub fn read_spec(file: &Path, alias: &str) -> Result<HostSpec> {
    let text = fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (start, end) = find_block(&lines, alias).ok_or_else(|| anyhow!("{alias} not found in {}", file.display()))?;
    let mut spec = HostSpec { alias: alias.to_string(), ..Default::default() };
    for l in &lines[start + 1..end] {
        let Some((k, v)) = split_kv(l) else { continue };
        let v = words(v).into_iter().next().unwrap_or_default();
        match k.as_str() {
            "hostname" if spec.hostname.is_empty() => spec.hostname = v,
            "user" if spec.user.is_empty() => spec.user = v,
            "port" if spec.port.is_empty() => spec.port = v,
            "identityfile" if spec.identity_file.is_empty() => spec.identity_file = v,
            _ => {}
        }
    }
    Ok(spec)
}

/// True when the alias shares its `Host` line with other patterns.
pub fn is_shared(file: &Path, alias: &str) -> bool {
    let Ok(text) = fs::read_to_string(file) else { return false };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    find_block(&lines, alias).and_then(|(s, _)| host_patterns(&lines[s])).is_some_and(|p| p.len() > 1)
}

// ------------------------------------------------------- pure transforms

fn render_block(spec: &HostSpec) -> Vec<String> {
    let mut out = vec![format!("Host {}", spec.alias)];
    for (k, v) in spec.directives() {
        out.push(format!("    {k} {v}"));
    }
    out
}

/// Append a new block. Fails if the alias already exists in this file.
pub fn add_to(text: &str, spec: &HostSpec) -> Result<String> {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    if find_block(&lines, &spec.alias).is_some() {
        bail!("A machine named {} already exists", spec.alias);
    }
    let mut out = lines;
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if !out.is_empty() {
        out.push(String::new());
    }
    out.extend(render_block(spec));
    Ok(out.join("\n") + "\n")
}

/// Rewrite the managed keys of `old_alias`'s block, keeping every other line
/// (comments, ServerAliveInterval, ...) exactly as it was.
pub fn edit_in(text: &str, old_alias: &str, spec: &HostSpec) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (start, mut end) = find_block(&lines, old_alias).ok_or_else(|| anyhow!("{old_alias} not found"))?;
    let pats = host_patterns(&lines[start]).unwrap_or_default();
    if pats.len() > 1 {
        bail!("{old_alias} shares its Host line with other machines; edit that file by hand");
    }
    if spec.alias != old_alias && find_block(&lines, &spec.alias).is_some() {
        bail!("A machine named {} already exists", spec.alias);
    }
    let indent: String = lines[start].chars().take_while(|c| c.is_whitespace()).collect();
    lines[start] = format!("{indent}Host {}", spec.alias);

    let wanted: Vec<(&str, Option<&str>)> = vec![
        ("HostName", Some(spec.hostname.as_str())),
        ("User", Some(spec.user.as_str()).filter(|v| !v.is_empty())),
        ("Port", Some(spec.port.as_str()).filter(|v| !v.is_empty())),
        ("IdentityFile", Some(spec.identity_file.as_str()).filter(|v| !v.is_empty())),
    ];
    let body_indent = lines[start + 1..end]
        .iter()
        .find(|l| split_kv(l).is_some())
        .map(|l| l.chars().take_while(|c| c.is_whitespace()).collect::<String>())
        .unwrap_or_else(|| "    ".to_string());

    for (key, value) in wanted {
        let lk = key.to_ascii_lowercase();
        let found: Vec<usize> =
            (start + 1..end).filter(|&i| split_kv(&lines[i]).is_some_and(|(k, _)| k == lk)).collect();
        match (found.first(), value) {
            (Some(&i), Some(v)) => {
                let ind: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
                lines[i] = format!("{ind}{key} {v}");
                // Drop duplicates of single-valued keys (IdentityFile may repeat).
                if key != "IdentityFile" {
                    for &j in found[1..].iter().rev() {
                        lines.remove(j);
                        end -= 1;
                    }
                }
            }
            (Some(_), None) => {
                for &j in found.iter().rev() {
                    lines.remove(j);
                    end -= 1;
                }
            }
            (None, Some(v)) => {
                // Insert after the last directive so the block stays tidy.
                let at = (start + 1..end).rev().find(|&i| split_kv(&lines[i]).is_some()).map_or(start + 1, |i| i + 1);
                lines.insert(at, format!("{body_indent}{key} {v}"));
                end += 1;
            }
            (None, None) => {}
        }
    }
    Ok(lines.join("\n") + "\n")
}

/// Remove `alias`. A block of its own is deleted; an alias sharing a `Host`
/// line only loses its name from that line.
pub fn delete_in(text: &str, alias: &str) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let (start, end) = find_block(&lines, alias).ok_or_else(|| anyhow!("{alias} not found"))?;
    let pats = host_patterns(&lines[start]).unwrap_or_default();
    if pats.len() > 1 {
        let indent: String = lines[start].chars().take_while(|c| c.is_whitespace()).collect();
        let rest: Vec<String> = pats
            .into_iter()
            .filter(|p| p != alias)
            .map(|p| if p.contains(' ') { format!("\"{p}\"") } else { p })
            .collect();
        lines[start] = format!("{indent}Host {}", rest.join(" "));
    } else {
        lines.drain(start..end);
        // Collapse the blank lines left at the seam.
        while start < lines.len()
            && lines[start].trim().is_empty()
            && (start == 0 || lines[start - 1].trim().is_empty())
        {
            lines.remove(start);
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    Ok(if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" })
}

// ------------------------------------------------------------- file I/O

/// Resolve symlinks so dotfile-managed configs are edited at their source.
fn real_path(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(e) => Err(e).with_context(|| format!("cannot resolve {}", path.display())),
    }
}

/// Read, transform, and atomically replace a config file.
pub fn modify(path: &Path, f: impl FnOnce(&str) -> Result<String>) -> Result<PathBuf> {
    let real = real_path(path)?;
    let (before, mode) = match fs::read_to_string(&real) {
        Ok(t) => (t, fs::metadata(&real)?.permissions().mode() & 0o777),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), 0o600),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", real.display())),
    };
    let after = f(&before)?;
    let dir = real.parent().ok_or_else(|| anyhow!("bad path {}", real.display()))?;
    fs::create_dir_all(dir)?;
    let name = real.file_name().unwrap_or_default().to_string_lossy().to_string();
    let tmp = dir.join(format!(".{name}.drawer-tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("cannot write in {}", dir.display()))?;
    let write = (|| -> Result<()> {
        file.write_all(after.as_bytes())?;
        file.sync_all()?;
        // Re-check right before replacing: never clobber someone else's edit.
        let now = fs::read_to_string(&real).unwrap_or_default();
        if now != before {
            bail!("{} changed on disk; reload (r) and try again", real.display());
        }
        if !before.is_empty() {
            let bak = dir.join(format!("{name}.drawer.bak"));
            fs::write(&bak, &before)?;
            fs::set_permissions(&bak, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, &real)?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write?;
    Ok(real)
}

/// Where new machines are written: the file that already defines the most
/// machines (so they live with the rest, e.g. in dotfiles), else
/// `~/.ssh/config.d/drawer.conf`, else the main config.
pub fn add_target(hosts: &[(String, PathBuf)]) -> PathBuf {
    let mut counts: Vec<(PathBuf, usize)> = Vec::new();
    for (_, f) in hosts {
        match counts.iter_mut().find(|(p, _)| p == f) {
            Some(c) => c.1 += 1,
            None => counts.push((f.clone(), 1)),
        }
    }
    // Most hosts wins; on a tie, the first file seen.
    let mut best: Option<(PathBuf, usize)> = None;
    for (p, n) in counts {
        if best.as_ref().is_none_or(|(_, b)| n > *b) {
            best = Some((p, n));
        }
    }
    if let Some((p, _)) = best {
        return p;
    }
    if crate::hosts::config_override().is_none() && includes_config_d() {
        crate::hosts::ssh_dir().join("config.d").join("drawer.conf")
    } else {
        crate::hosts::main_config()
    }
}

fn includes_config_d() -> bool {
    let Ok(text) = fs::read_to_string(crate::hosts::main_config()) else { return false };
    text.lines().any(|l| matches!(split_kv(l), Some((k, v)) if k == "include" && v.contains("config.d/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(alias: &str, host: &str) -> HostSpec {
        HostSpec { alias: alias.into(), hostname: host.into(), ..Default::default() }
    }

    const SAMPLE: &str = "\
# lab machines
Host pi
    HostName 192.168.1.10
    User admin
    ServerAliveInterval 60

# the big one
Host nas backup
    HostName 192.168.1.30

Host *.lan
    User nv
";

    #[test]
    fn rejects_injection() {
        let mut s = spec("pve", "192.168.1.11");
        s.user = "root\n    ProxyCommand touch /tmp/pwned".into();
        assert!(s.validated().is_err());
        let s = spec("pve", "1.2.3.4 -oProxyCommand=x");
        assert!(s.validated().is_err());
        let s = spec("-oProxyCommand=x", "1.2.3.4");
        assert!(s.validated().is_err());
        let s = spec("a*b", "1.2.3.4");
        assert!(s.validated().is_err());
        let mut s = spec("pve", "h");
        s.port = "0".into();
        assert!(s.validated().is_err());
        s.port = "70000".into();
        assert!(s.validated().is_err());
        s.port = "22".into();
        assert!(s.validated().is_ok());
        assert!(spec("ok-host_1.lan", "[fe80::1%eth0]").validated().is_ok());
    }

    #[test]
    fn adds_block() {
        let mut s = spec("proxmox", "192.168.1.11");
        s.user = "root".into();
        s.port = "22".into();
        let out = add_to(SAMPLE, &s).unwrap();
        assert!(out.ends_with("User nv\n\nHost proxmox\n    HostName 192.168.1.11\n    User root\n    Port 22\n"));
        assert!(add_to(SAMPLE, &spec("pi", "x")).is_err());
        assert_eq!(add_to("", &spec("a", "b")).unwrap(), "Host a\n    HostName b\n");
    }

    #[test]
    fn edits_in_place_keeping_other_lines() {
        let mut s = spec("pi4", "192.168.1.12");
        s.port = "2222".into();
        let out = edit_in(SAMPLE, "pi", &s).unwrap();
        assert!(out.contains("Host pi4\n    HostName 192.168.1.12\n    ServerAliveInterval 60\n    Port 2222\n"));
        assert!(!out.contains("User admin"));
        assert!(out.starts_with("# lab machines\n"));
        assert!(out.contains("# the big one\nHost nas backup"));
        // shared Host line refused
        assert!(edit_in(SAMPLE, "nas", &spec("nas", "x")).is_err());
        // rename onto an existing alias refused
        assert!(edit_in(SAMPLE, "pi", &spec("nas", "x")).is_err());
    }

    #[test]
    fn deletes_block_and_keeps_neighbours() {
        let out = delete_in(SAMPLE, "pi").unwrap();
        assert_eq!(
            out,
            "# lab machines\n# the big one\nHost nas backup\n    HostName 192.168.1.30\n\nHost *.lan\n    User nv\n"
        );
        let out = delete_in(SAMPLE, "backup").unwrap();
        assert!(out.contains("Host nas\n    HostName 192.168.1.30"));
        assert!(delete_in(SAMPLE, "nope").is_err());
    }

    #[test]
    fn reads_spec() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("c");
        fs::write(&f, SAMPLE).unwrap();
        let s = read_spec(&f, "pi").unwrap();
        assert_eq!((s.hostname.as_str(), s.user.as_str(), s.port.as_str()), ("192.168.1.10", "admin", ""));
        assert!(is_shared(&f, "nas"));
        assert!(!is_shared(&f, "pi"));
    }

    #[test]
    fn modify_follows_symlink_keeps_backup_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let real = d.join("dotfiles-lab.conf");
        fs::write(&real, SAMPLE).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        let link = d.join("lab.conf");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let written = modify(&link, |t| add_to(t, &spec("pve", "192.168.1.11"))).unwrap();
        assert_eq!(written, fs::canonicalize(&real).unwrap());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "symlink preserved");
        assert!(fs::read_to_string(&link).unwrap().contains("Host pve"));
        assert_eq!(fs::read_to_string(d.join("dotfiles-lab.conf.drawer.bak")).unwrap(), SAMPLE);
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);

        // A failing transform leaves the file untouched.
        let before = fs::read_to_string(&real).unwrap();
        assert!(modify(&link, |_| bail!("nope")).is_err());
        assert_eq!(fs::read_to_string(&real).unwrap(), before);
    }

    #[test]
    fn modify_creates_new_file_private() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("config.d/drawer.conf");
        modify(&f, |t| add_to(t, &spec("a", "b"))).unwrap();
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
