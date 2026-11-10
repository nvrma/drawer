# Contributing to drawer

Thanks for helping out! Bug reports, ideas and pull requests are all welcome.

## Development

You need Rust 1.88 or newer and an OpenSSH client.

```sh
git clone https://github.com/nvrma/drawer && cd drawer
cargo run
```

Before you open a pull request, make sure these pass. CI runs the same checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

To try drawer against a test setup without touching your own `~/.ssh/config`, point it at another config file:

```sh
DRAWER_CONFIG=/tmp/test-ssh-config cargo run
```

## Where things live

| File | What it does |
|---|---|
| `src/app.rs` | app state, keys, background tasks |
| `src/ui.rs` | drawing |
| `src/onboarding.rs` | the welcome tour |
| `src/session.rs` | the `ssh` process in a pseudo-terminal, acting as control master |
| `src/remote.rs` | SFTP over the control socket, listing, key install |
| `src/transfer.rs` | downloads and uploads |
| `src/configedit.rs` | safe edits to ssh config files |
| `src/hosts.rs` | reading `~/.ssh/config` |
| `src/download.rs` | local file naming and no-overwrite logic |

The interface follows [DESIGN.md](DESIGN.md). In short:
- use only terminal palette colours, never fixed RGB;
- yellow means "you can press this now";
- each machine keeps one colour everywhere;
- no animations.

## Pull requests

- Keep each change focused, and describe what it does and why.
- Add tests for new logic, especially anything that touches files, paths or ssh config.
- Security-sensitive changes (transfers, config edits, anything that runs remote commands) get extra review. Please explain how your change stays safe.

## Releasing (maintainers)

1. Bump `version` in `Cargo.toml` and add a `CHANGELOG.md` entry.
2. Commit, then tag and push: `git tag v0.x.y && git push origin v0.x.y`.
3. The release workflow builds the binaries, checksums them and attaches them to the GitHub release. `install.sh` picks up the new version automatically.
