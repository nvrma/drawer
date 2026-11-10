# Changelog

All notable changes to drawer are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

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

[0.1.0]: https://github.com/nvrma/drawer/releases/tag/v0.1.0
