use crate::args::g5k::G5kCommands;
use crate::args::generate::GenerateCommand;
use crate::args::plot::PlotCommand;
use crate::args::run::RunCommand;
use crate::models::hardware_resources::HardwareResources;
use crate::models::nic::NicType;
use crate::models::protocol::RoutingProtocol;
use clap::{Parser, Subcommand};
use clap_verbosity_flag::Verbosity;
use strum::Display;
use crate::args::compute_results::ComputeResultsCommand;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub verbosity: Verbosity
}

#[derive(Debug, Clone, Subcommand, Display)]
pub enum Command {
    /// Generate experiment files
    Generate(GenerateCommand),
    /// Run experiment files
    Run(RunCommand),
    /// Compute results before plotting
    Compute(ComputeResultsCommand),
    /// Plot experiment results
    Plot(PlotCommand),
    #[clap(subcommand)]
    /// Upload experiments to Grid'5000 and reserve a node to run them
    G5k(G5kCommands),
    /// Prints commands and args tree
    Tree,
}


#[derive(Debug, Clone, clap::Args)]
pub struct ExperimentSelectionArgs {
    /// OS to use. All if absent
    #[arg(short, long)]
    pub os: Option<String>,

    /// Test to use. All if absent
    #[arg(long)]
    pub test_batch: Option<String>,

    /// Resources to use. All if absent
    #[arg(short, long)]
    pub resources: Option<HardwareResources>,

    /// NIC to use. All if absent
    #[arg(short, long)]
    pub nic: Option<NicType>,

    /// Topology to use. All if absent
    #[arg(short, long)]
    pub topology: Option<String>,

    /// Routing protocol to use. All if absent
    #[arg(short, long)]
    pub protocol: Option<RoutingProtocol>,

    /// Run the experiment with the failure event listed in the topology
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub failures: Option<bool>,
}