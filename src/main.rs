pub mod orb;
pub mod ore;
pub mod hmm;
pub mod ws_server;
pub mod log_macros;
pub mod rpc;
pub mod utils;
pub mod worker;

use std::path::PathBuf;
use colored::{Colorize, CustomColor};
use clap::Parser;

use crate::{utils::read_filenames, worker::{run_multiple, run_single}};

pub const ORE_LOG: &str = "[ORE]";
pub const ORB_LOG: &str = "[ORB]";

pub fn ore_log() -> colored::ColoredString {
    ORE_LOG.custom_color(CustomColor { r: 230, g: 200, b: 125 })
}

pub fn orb_log() -> colored::ColoredString {
    ORB_LOG.custom_color(CustomColor { r: 51, g: 89, b: 246 })
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    paths: PathBuf,

    #[arg(long)]
    ore: bool,

    #[arg(long)]
    orb: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().expect("Failed to load env");

    let args = Args::parse();

    let files = read_filenames(&args.paths);
    info_log!("Loaded {} miners", files.len());

    if args.orb && args.ore {
        run_multiple(files).await?;
    } else if args.ore {
        run_single(files, 0).await?;
    } else {
        run_single(files, 1).await?;
    }

    Ok(())
}