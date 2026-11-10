mod app;
mod configedit;
mod download;
mod hosts;
mod keys;
mod onboarding;
mod remote;
mod session;
mod theme;
mod transfer;
mod ui;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};

const HELP: &str = "\
drawer — your SSH machines, shell and files in one terminal screen

USAGE:
    drawer              start
    drawer --welcome    show the welcome tour again
    drawer --version    print the version
    drawer --help       print this help

Machines are the Host entries in ~/.ssh/config (Include is followed).
Connections use your system OpenSSH client, so known_hosts checking,
keys, ssh-agent and ProxyJump all work as they do with `ssh`.

ENVIRONMENT:
    DRAWER_CONFIG=<file>   use another ssh config (like ssh -F)
    DRAWER_NO_ICONS=1      plain symbols instead of Nerd Font icons

Press ? inside the app for keys.
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut welcome = onboarding::should_show();
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("drawer {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--welcome" => welcome = true,
            other => {
                eprintln!("drawer: unknown argument '{other}'\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let result = app::run(&mut terminal, welcome).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}
