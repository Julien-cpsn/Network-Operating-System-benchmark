pub(crate) mod models;
pub(crate) mod utils;
pub(crate) mod args;
pub(crate) mod commands;

use crate::commands::generate::generate;
use crate::commands::run::run;
use tracing::{info};
use once_cell::sync::{Lazy, OnceCell};
use std::path::PathBuf;
use clap::{CommandFactory, Parser};
use futures::{FutureExt};
use crate::args::args::{Args, Command};
use crate::args::g5k::G5kCommands;
use crate::args::generate::GenerateSubcommand;
use crate::commands::compute_results::compute_results;
use crate::commands::g5k::deploy::deploy_g5k;
use crate::commands::g5k::harvest::harvest_g5k;
use crate::commands::plot::plot;
use crate::commands::tree::print_tree;
use crate::utils::log::setup_global_logger;

pub static ARGS: Lazy<Args> = Lazy::new(|| Args::parse());

pub static QEMU_BINARY: OnceCell<String> = OnceCell::new();
pub static QEMU_IMG_BINARY: OnceCell<String> = OnceCell::new();
pub static QEMU_WORK_DIR: OnceCell<PathBuf> = OnceCell::new();
pub static QEMU_ENABLE_KVM: OnceCell<bool> = OnceCell::new();
pub static GUEST_IMAGE_PATH: OnceCell<PathBuf> = OnceCell::new();

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    setup_global_logger()?;

    handle_command(&ARGS.command).await?;

    Ok(())
}

async fn handle_command(command: &Command) -> anyhow::Result<()> {
    info!(target: "main", "===== {} command =====", command);

    match &command {
        Command::Generate(generate_command) => {
            generate(generate_command.clone())?;

            if let GenerateSubcommand::Run(run_command) = &generate_command.command {
                handle_command(&Command::Run(run_command.clone())).boxed_local().await?;
            }
        },
        Command::Run(run_command) => run(run_command.clone()).await?,
        Command::Compute(compute_results_command) => compute_results(compute_results_command.clone())?,
        Command::Plot(plot_command) => plot(plot_command.clone())?,
        Command::G5k(g5k_command) => match g5k_command {
            G5kCommands::Deploy(deploy_command) => deploy_g5k(deploy_command.clone()).await?,
            G5kCommands::Harvest(harvest_command) => harvest_g5k(harvest_command.clone()).await?,
        },
        Command::Tree => {
            let mut cmd = Args::command()
                .name("Benchmark and plot performances for router and network OSes");
            cmd.build();
            print_tree(&cmd, 0);
        }
    }

    Ok(())
}
