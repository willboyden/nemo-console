//! `nemo-console`: serve (no arguments, or any arguments that are not a subcommand, like Python), `mint-key
//! [admin|viewer]`, `open`, `stop`. Port of `main.py` `run()` / `mint_key()`.

use std::process::ExitCode;

use nemo_console::config::Settings;
use nemo_console::domain::models::Role;
use nemo_console::services::auth::{AuthService, write_private_file};
use nemo_console::services::common::KNOWN;
use nemo_console::services::store::Store;

#[allow(clippy::print_stdout, clippy::print_stderr)]
fn mint_key(argv: &[String]) -> i32 {
    let role = argv.first().map_or("viewer", String::as_str);
    let role = match role {
        "admin" => Role::Admin,
        "viewer" => Role::Viewer,
        _ => {
            eprintln!("usage: nemo-console mint-key [admin|viewer]");
            return 2;
        }
    };
    let cfg = match Settings::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let store = match Store::open(&cfg.state_dir) {
        Ok(s) => std::sync::Arc::new(s),
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let auth = AuthService {
        store,
        state_dir: cfg.state_dir.clone(),
        trust_loopback: false,
    };
    let (kid, key) = match auth.mint(role, "minted via CLI") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let path = cfg
        .state_dir
        .join(format!("minted-{}-{kid}.key", role.as_str()));
    if let Err(e) = write_private_file(&path, &format!("{key}\n")) {
        eprintln!("{e}");
        return 1;
    }
    KNOWN.add(&key);
    println!(
        "{} key written to {} (0600); it is not shown here",
        role.as_str(),
        path.display()
    );
    0
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let rest = args.get(2..).unwrap_or(&[]);
    let code = match args.get(1).map(String::as_str) {
        Some("mint-key") => mint_key(rest),
        Some("open") => nemo_console::desktop::open_console(rest, None),
        Some("stop") => nemo_console::desktop::stop_console(rest, None),
        _ => nemo_console::server::run(),
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
