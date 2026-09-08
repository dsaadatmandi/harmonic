use anyhow::{Context, Result};
use clap::Parser;

use harmonic::client::sync::{run_sync, trigger_sync_task};
use harmonic::client::watcher::{
    QUEUE_CHECK_SEC_INTERVAL_SEC, pop_sync_trigger, start_scheduler, start_watcher,
};
use harmonic::sync;
use harmonic::utils::tracing::{shutdown_tracer, tracing_orchestrator};
use std::path::PathBuf;
use tokio::time::Duration;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "harmonic-client")]
#[command(about = "Harmonic file synchronization client", long_about = None)]
struct Args {
    /// Bootstrap certificate from server
    #[arg(long)]
    bootstrap: bool,
    /// Use schedule-based sync mode
    #[arg(long)]
    schedule: bool,
    /// Use event-based sync mode
    #[arg(long)]
    event_based: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args = Args::parse();

    let config = sync::load_config().context("Failed to load config")?;

    tracing_orchestrator(&config.log_level);

    if args.event_based {
        let _watcher_task = start_watcher(PathBuf::from(&config.sync_path), &config);
    }

    if args.schedule {
        let _scheduler_task = start_scheduler(&config);
    }

    let manual = !args.event_based && !args.schedule;

    if manual {
        info!("Triggering manual sync");
        let result = run_sync(&config, args.bootstrap)
            .await
            .context("Initial sync task failed");

        shutdown_tracer().await;

        result?;
        Ok(())
    } else {
        let mut queue_check_interval =
            tokio::time::interval(Duration::from_secs(QUEUE_CHECK_SEC_INTERVAL_SEC));

        // the bootstrap flag applies to the first triggered sync only
        let mut force_bootstrap = args.bootstrap;

        loop {
            if pop_sync_trigger().await {
                trigger_sync_task(config.clone(), force_bootstrap)
                    .await
                    .context("Sync task failed")?;
                force_bootstrap = false;
            }
            queue_check_interval.tick().await;
        }
    }
}
