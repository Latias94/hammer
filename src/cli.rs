use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "hammer",
    version,
    about = "A command-line toolbox for AI-assisted coding"
)]
pub struct Cli {
    #[arg(
        global = true,
        long,
        short = 'n',
        help = "Print actions without executing"
    )]
    pub dry_run: bool,

    #[arg(
        global = true,
        long,
        short = 'v',
        action = clap::ArgAction::Count,
        help = "Increase verbosity (-v, -vv)"
    )]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Git pull all repositories under a path")]
    Gp(crate::commands::gp::GpArgs),
}
