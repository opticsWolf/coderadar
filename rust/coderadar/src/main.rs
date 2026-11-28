//! The single Rust binary: `coderadar <command>` and `coderadar mcp serve`.

use std::process::ExitCode;

fn main() -> ExitCode {
    // clap's derive builds a deeply nested `Command` for this many
    // subcommands; the default 1 MiB main-thread stack overflows while it
    // does. Run the same entry point on a roomy stack instead of trimming
    // the CLI surface.
    match std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run)
    {
        Ok(handle) => handle.join().unwrap_or(ExitCode::FAILURE),
        Err(_) => run(),
    }
}

fn run() -> ExitCode {
    match coderadar::cli::run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) if failure.code == 0 => {
            println!("{}", failure.message);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("{}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}
