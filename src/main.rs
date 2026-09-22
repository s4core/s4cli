mod cli;
mod commands;
mod config;
mod output;
mod python;
mod s3;
mod target;
mod time;
mod util;

use std::env;

use commands::Context;

fn main() {
    restore_default_sigpipe();
    let result = run();
    util::cleanup_temp_root();
    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Rust ignores SIGPIPE, which turns `s4 ... | head` into a "failed printing to
/// stdout" panic; restore the default so the process exits quietly like other CLIs.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    // SAFETY: runs once at startup, before any other thread exists.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        cli::print_help();
        return Ok(());
    }

    let (opts, rest) = cli::parse_globals(args)?;
    let Some(command) = rest.first() else {
        cli::print_help();
        return Ok(());
    };
    match command.as_str() {
        "--help" | "-h" => {
            cli::print_help();
            return Ok(());
        }
        "--version" | "-v" | "version" => {
            println!("s4 {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }

    let config_path = config::resolve_config_path(opts.config_dir.as_deref())?;
    let config = config::load_config(&config_path)?;
    if opts.debug {
        eprintln!("[debug] config: {}", config_path.display());
    }

    let mut ctx = Context {
        config,
        config_path,
        json: opts.json,
        debug: opts.debug,
        http: opts.http,
    };
    commands::dispatch(&mut ctx, &rest)
}
