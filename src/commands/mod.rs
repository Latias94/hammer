pub mod gp;

use anyhow::Result;

use crate::cli::Commands;
use crate::context::Context;

pub fn run(command: Commands, ctx: &Context) -> Result<()> {
    match command {
        Commands::Gp(args) => gp::run(&args, ctx),
    }
}
