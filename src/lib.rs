mod cli;
mod commands;
mod context;

use anyhow::Result;
use clap::Parser;

pub fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    let ctx = context::Context::new(cli.dry_run, cli.verbose);
    commands::run(cli.command, &ctx)
}
