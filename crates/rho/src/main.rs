//! The `rho` binary — a full-parity Rust port of the `tau` coding agent.
//!
//! Milestone M0 provides only a clap skeleton: it prints the version and accepts
//! a `-p/--print` flag whose non-interactive print mode is not implemented yet.
//! The real CLI surface is wired up from M4a onward.

use clap::Parser;

/// A minimalist Pi-style coding-agent harness (Rust port of tau).
#[derive(Debug, Parser)]
#[command(name = "rho", version, about, long_about = None)]
struct Cli {
    /// Run a single prompt in non-interactive print mode (not implemented yet).
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    if cli.print.is_some() {
        eprintln!("print mode: not implemented yet");
        std::process::exit(1);
    }

    // No subcommand yet: report the version, matching the M0 skeleton contract.
    println!("rho {}", env!("CARGO_PKG_VERSION"));
}
