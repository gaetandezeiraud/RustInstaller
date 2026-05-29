mod args;
mod embed;
mod keygen;
mod pack;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = args::Cli::parse();
    match &cli.command {
        args::Command::Keygen(a) => keygen::run(a),
        args::Command::Pack(a) => pack::run(a),
    }
}
