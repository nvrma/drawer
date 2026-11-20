# Changelog

All notable changes to drawer are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-11-20

### Fixed
- **Scrolling the shell.** The mouse wheel now scrolls the shell's history. Before, many terminals (Ghostty, kitty and others) turned the wheel into arrow keys and kept `Shift+PgUp` for themselves, so the shell pane couldn't be scrolled at all.

### Added
- **Mouse support.**
  - Click a pane to focus it, and click a machine or file to select it. Double-click connects, opens a folder or previews a file.
  - The wheel scrolls lists, previews and the shell. In full-screen programs such as `less` it moves through the program, as in any terminal.
  - Drag in the shell to select text; it's copied to your clipboard. Double-click copies a word, triple-click a line. `Shift`+drag still uses your terminal's own selection.
  - Programs that use the mouse (`htop`, `vim` with `mouse=a`, `tmux`) get your clicks and scrolls.
  - Every key hint in the footer and in popups can be clicked.
  - Set `DRAWER_NO_MOUSE=1` to leave the mouse to your terminal.
- **More ways to scroll back:** `Alt+PgUp`/`Alt+PgDn` work in terminals that keep `Shift+PgUp`. While scrolled back, `↑`/`↓`/`PgUp`/`PgDn` move through the history and `Esc` returns to the live screen.
- **Zoom** (`Alt+z`): the shell fills the whole window. Press it again to go back.
- **Port forwarding** (`f` on a connected machine): forward a local port to the machine (`3000`, `8080:3000`, `8080:10.0.0.5:80`), or start a SOCKS proxy (`socks 1080`). It uses the connection you already have, so there's no second login. Forwards listen on `127.0.0.1` only. `o` opens one in your browser and `x` stops it. Active forwards show in the shell's title.
- **File preview** (`p`, `Enter` or a double-click on a file): read a text file without downloading it, with line numbers and scrolling. Binary files are detected. `d` downloads from the preview.
- **File management:** `n` new folder, `R` rename, `D` delete (asks first, and only a literal `y` confirms). Nothing is ever overwritten. A deleted folder takes everything in it, but links inside are removed as links and never followed.
- **Filter and sort files:** `/` filters the current folder as you type, and `s` sorts by name, newest or largest.
- `y` copies the selected file's remote path. `c` types `cd '<folder>'` into the shell for the folder you're browsing (you press Enter).

### Changed
- `Enter` on a file now previews it.
- The footer shows the keys for what you're doing (scrolled back, filtering, connected machine).

## [0.1.0] - 2026-11-10

First release.

### Added
- Three-pane layout: machines from `~/.ssh/config` (with `Include`), a live shell, and the selected machine's files.
- Sessions run through the system OpenSSH client as a control master. The file browser shares that connection over SFTP, so there's one login per machine.
- File or folder download to `~/Downloads`, and file or folder upload, with confirmation, progress and cancel. Nothing is ever overwritten.
- Add, edit, delete and search machines inside the app, with validated fields, atomic writes and backups.
- `K`: install your public key on a connected machine.
- First-run welcome tour (`drawer --welcome` shows it again).
- Uses the terminal's own colour palette, so it follows your theme.
- One-line installer with checksum verification and prebuilt binaries for Linux and macOS (x86_64 and ARM64).

[0.2.0]: https://github.com/nvrma/drawer/releases/tag/v0.2.0
[0.1.0]: https://github.com/nvrma/drawer/releases/tag/v0.1.0
