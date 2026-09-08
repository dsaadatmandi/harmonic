use futures::{
    StreamExt,
    channel::mpsc::{unbounded, UnboundedReceiver},
};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::instrument::Instrumented;
use tracing::{Instrument, Span, debug, error, info, instrument};

use crate::sync;

fn async_watcher() -> notify::Result<(RecommendedWatcher, UnboundedReceiver<notify::Result<Event>>)> {
    let (tx, rx) = unbounded();

    // Automatically select the best implementation for your platform.
    // You can also access each implementation directly e.g. INotifyWatcher.
    let watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.unbounded_send(res);
        },
        Config::default(),
    )?;

    Ok((watcher, rx))
}

pub async fn async_watch<P: AsRef<Path>>(path: P) -> notify::Result<(RecommendedWatcher, UnboundedReceiver<notify::Result<Event>>)> {
    let (mut watcher, rx) = async_watcher()?;

    // Add a path to be watched. All files and directories at that path and
    // below will be monitored for changes.
    watcher.watch(path.as_ref(), RecursiveMode::Recursive)?;

    Ok((watcher, rx))
}

static QUEUE: once_cell::sync::Lazy<Arc<tokio::sync::Mutex<VecDeque<bool>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(tokio::sync::Mutex::new(VecDeque::new())));

pub const QUEUE_CHECK_SEC_INTERVAL_SEC: u64 = 10;

#[instrument(skip(config), fields(watch_path = %p.display()))]
pub fn start_watcher(p: std::path::PathBuf, config: &sync::Config) -> Instrumented<JoinHandle<()>> {
    info!("Starting file system watcher");
    let c = config.clone();
    tokio::spawn(async move {
        let (_watcher, mut rx) = match async_watch(p).await {
            Ok((w, rx)) => (w, rx),
            Err(e) => {
                error!("Error in creating file watcher: {:#}", e);
                return;
            }
        };

        let mut change_score: u64 = 0;

        while let Some(Ok(event)) = rx.next().await {
            change_score += calculate_change_score(event.kind, &c);
            if should_trigger_sync(change_score, &c) {
                info!("Sufficient changes accrued. Triggering sync job.");
                QUEUE.lock().await.push_back(true);
                change_score = 0;
            }
        }
    })
    .instrument(Span::current())
}

pub fn start_scheduler(config: &sync::Config) -> Instrumented<JoinHandle<()>> {
    debug!("Starting scheduler");
    let mut delay_interval =
        tokio::time::interval(tokio::time::Duration::from_secs(config.schedule_delay));
    tokio::spawn(async move {
        loop {
            QUEUE.lock().await.push_back(true);
            delay_interval.tick().await;
        }
    })
    .instrument(Span::current())
}

/// Pops a pending sync trigger from the queue, clearing anything accrued
pub async fn pop_sync_trigger() -> bool {
    let mut queue = QUEUE.lock().await;
    let trigger = queue.pop_front().is_some();
    if trigger {
        queue.clear();
    }
    trigger
}

fn calculate_change_score(event_kind: EventKind, config: &sync::Config) -> u64 {
    match event_kind {
        EventKind::Modify(_) => config.modify_weight,
        EventKind::Remove(_) => config.remove_weight,
        EventKind::Create(_) => config.create_weight,
        _ => {
            info!("Unmatched event of type {:?}", event_kind);
            0
        }
    }
}

fn should_trigger_sync(score: u64, config: &sync::Config) -> bool {
    score > config.sync_threshold
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind, RemoveKind};

    #[test]
    fn test_calculate_change_score() {
        let config = sync::Config::default();

        assert_eq!(
            calculate_change_score(EventKind::Modify(ModifyKind::Any), &config),
            config.modify_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Remove(RemoveKind::Any), &config),
            config.remove_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Create(CreateKind::Any), &config),
            config.create_weight
        );
        assert_eq!(
            calculate_change_score(EventKind::Access(notify::event::AccessKind::Any), &config),
            0
        );
    }

    #[test]
    fn test_should_trigger_sync() {
        let config = sync::Config::default();

        assert!(!should_trigger_sync(config.sync_threshold, &config));
        assert!(should_trigger_sync(config.sync_threshold + 1, &config));
        assert!(!should_trigger_sync(config.sync_threshold - 1, &config));
        assert!(!should_trigger_sync(0, &config));
    }

    #[tokio::test]
    async fn test_pop_sync_trigger_clears_accrued_triggers() {
        QUEUE.lock().await.push_back(true);
        QUEUE.lock().await.push_back(true);
        QUEUE.lock().await.push_back(true);

        assert!(pop_sync_trigger().await);

        let queue = QUEUE.lock().await;
        assert!(queue.is_empty(), "accrued triggers must be cleared");
    }
}
