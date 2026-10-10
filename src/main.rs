//! nora: a fast, efficient, maintainable Linux desktop terminal.
//!
//! Bootstrap stage (N00): the binary identifies itself and its version.
//! No terminal UI exists yet; that work starts at N01.

use std::process::ExitCode;

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: nora [OPTIONS]\n\n\
A fast, efficient, maintainable Linux desktop terminal.\n\
\
Options:\n\
  -h, --help       Print this help message\n\
  -V, --version    Print nora's name and version\n";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [] => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        [arg] if *arg == "--help" || *arg == "-h" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        [arg] if *arg == "--version" || *arg == "-V" => {
            println!("{NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        [first, ..] => {
            eprintln!("{NAME} {VERSION}: unrecognized option: {first}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_reports_name_and_version() {
        assert_eq!(format!("{NAME} {VERSION}"), format!("nora {VERSION}"));
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
