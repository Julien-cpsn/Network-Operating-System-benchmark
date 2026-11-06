use crate::args::args::ExperimentSelectionArgs;

#[derive(Debug, Clone, clap::Args)]
pub struct AnalyzeCommand {
    #[clap(flatten)]
    pub experiment_selection: ExperimentSelectionArgs,

    /// Reuse the tables of a previous analysis (experimentation/analysis/data/runs.csv) instead of re-reading every result
    #[arg(long)]
    pub reuse: bool,

    /// Skip the gradient boosting cross-check (the slowest step)
    #[arg(long)]
    pub no_model: bool,

    /// Only write the tables and the report, no figure
    #[arg(long)]
    pub no_plots: bool,

    /// Number of parallel extraction processes. Default: number of CPUs - 1 (max 8)
    #[arg(long)]
    pub jobs: Option<usize>,

    /// Comma-separated metrics for the effect, model and ranking steps. Default: the key metrics
    #[arg(long)]
    pub metrics: Option<String>,

    /// Share of the scenarios an OS must cover to be included in the rankings
    #[arg(long, default_value_t = 0.8)]
    pub min_coverage: f64,

    /// Bootstrap resamples for the confidence intervals of the medians
    #[arg(long, default_value_t = 2000)]
    pub bootstrap: usize,
}
