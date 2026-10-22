use anyhow::{Context, Result};
use clap::Parser;

mod cli;
mod commands;
mod config;
mod fsutil;
mod index;
mod ingest;
mod pages;
#[cfg(test)]
mod test_support;
mod text;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let root = std::env::current_dir().context("failed to get current directory")?;
    commands::run(cli.command, &root, &mut std::io::stdout().lock())
}
