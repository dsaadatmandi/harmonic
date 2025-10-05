use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver},
};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;

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