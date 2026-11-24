//! The single Rust binary: `coderadar <command>` and `coderadar mcp serve`.

use std::process::ExitCode;

fn main() -> ExitCode {
    match coderadar::cli::run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
