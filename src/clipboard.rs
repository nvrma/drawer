//! Copy text to the system clipboard.
//!
//! Two routes, both best effort:
//! - OSC 52, which the terminal turns into a clipboard write. Works over ssh
//!   and in most modern terminals (Ghostty, kitty, WezTerm, Alacritty, foot,
//!   iTerm2, tmux with `set-clipboard on`).
//! - The platform's own tool (`pbcopy`, `wl-copy`, `xclip`, `xsel`) when drawer
//!   runs on the machine in front of you, for terminals that ignore OSC 52.

use std::io::Write;
use std::process::{Command, Stdio};

/// Largest selection sent over OSC 52; some terminals drop longer sequences.
const OSC52_MAX: usize = 100_000;

pub fn copy(text: &str) {
    if text.len() <= OSC52_MAX {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = out.flush();
    }
    if std::env::var_os("SSH_CONNECTION").is_none() {
        native(text);
    }
}

fn native(text: &str) {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
    } else if std::env::var_os("DISPLAY").is_some() {
        &[("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
    } else {
        &[]
    };
    for (cmd, args) in candidates {
        let child =
            Command::new(cmd).args(*args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        let Ok(mut child) = child else { continue };
        let text = text.to_string();
        // Write and reap off the UI thread; clipboard tools can linger.
        std::thread::spawn(move || {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
        return;
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("é/\n".as_bytes()), "w6kvCg==");
    }
}
