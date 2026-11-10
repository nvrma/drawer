//! Local side of a download: safe names, no overwrites, atomic finish.

use std::io;
use std::path::{Path, PathBuf};

/// The local Downloads folder (XDG), falling back to ~/Downloads.
pub fn downloads_dir() -> PathBuf {
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_else(|| PathBuf::from("."))
}

/// Turn an untrusted remote file name into a single safe path component.
/// Never yields an empty name, `.`/`..`, a separator, or control characters.
pub fn sanitize(remote_name: &str) -> String {
    let base = remote_name.rsplit('/').next().unwrap_or("");
    let cleaned: String = base.chars().map(|c| if c.is_control() || c == '/' || c == '\\' { '_' } else { c }).collect();
    let mut s = cleaned.trim().to_string();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        s = "download".to_string();
    }
    // Keep well under NAME_MAX (255 bytes) to leave room for " (n)".
    if s.len() > 200 {
        let mut cut = 200;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// `name` for n = 0, otherwise `stem (n).ext`.
pub fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({n}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({n})"),
    }
}

/// First name in `dir` that does not exist yet (for display before download).
pub fn preview_target(dir: &Path, name: &str) -> PathBuf {
    for n in 0..10_000 {
        let p = dir.join(numbered(name, n));
        if std::fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    dir.join(name)
}

/// Hidden in-progress file, created exclusively (never follows a symlink,
/// never clobbers).
pub fn part_path(dir: &Path, name: &str, id: u64) -> PathBuf {
    dir.join(format!(".{name}.{id}.drawer-part"))
}

/// Move a finished part file to its final name without ever overwriting an
/// existing file. Uses hard links so the "does it exist" check and the move
/// are one atomic step.
pub fn finalize(part: &Path, dir: &Path, name: &str) -> io::Result<PathBuf> {
    for n in 0..10_000 {
        let target = dir.join(numbered(name, n));
        match std::fs::hard_link(part, &target) {
            Ok(()) => {
                std::fs::remove_file(part)?;
                return Ok(target);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                // Filesystem without hard links: fall back to a checked rename.
                if std::fs::symlink_metadata(&target).is_ok() {
                    continue;
                }
                std::fs::rename(part, &target)?;
                return Ok(target);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many files with this name"))
}

pub fn tilde(p: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize("../../.bashrc"), ".bashrc");
        assert_eq!(sanitize(".."), "download");
        assert_eq!(sanitize("..."), "download");
        assert_eq!(sanitize(""), "download");
        assert_eq!(sanitize("a\nb\x1b[31m"), "a_b_[31m");
        assert_eq!(sanitize("dir/evil"), "evil");
        assert_eq!(sanitize("back\\slash"), "back_slash");
        assert_eq!(sanitize("  report.pdf "), "report.pdf");
        let long = "é".repeat(300);
        let s = sanitize(&long);
        assert!(s.len() <= 200 && s.is_char_boundary(s.len()));
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered("a.tar.gz", 0), "a.tar.gz");
        assert_eq!(numbered("a.tar.gz", 2), "a.tar (2).gz");
        assert_eq!(numbered("README", 1), "README (1)");
        assert_eq!(numbered(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn finalize_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(d.join("f.txt"), "old").unwrap();
        let part = part_path(d, "f.txt", 1);
        std::fs::write(&part, "new").unwrap();
        let out = finalize(&part, d, "f.txt").unwrap();
        assert_eq!(out, d.join("f (1).txt"));
        assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "new");
        assert!(!part.exists());
    }

    #[test]
    fn finalize_does_not_follow_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let victim = d.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, d.join("x")).unwrap();
        let part = part_path(d, "x", 2);
        std::fs::write(&part, "payload").unwrap();
        let out = finalize(&part, d, "x").unwrap();
        assert_eq!(out, d.join("x (1)"));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }
}
