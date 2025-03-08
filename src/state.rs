use std::sync::Arc;

use async_channel::Sender;
use futures::{Stream, StreamExt};
use waybar_cffi::gtk::glib;

use crate::{
    config::Config,
    error::Error,
    icon,
    niri::{Niri, Snapshot, WindowStream},
    notify::{self, Notification},
};

/// Global state for the taskbar.
#[derive(Debug, Clone)]
pub struct State(Arc<Inner>);

impl State {
    /// Instantiates the global state.
    pub fn new(config: Config) -> Self {
        Self(Arc::new(Inner {
            config,
            icon_cache: icon::Cache::default(),
            niri: Niri::new(),
        }))
    }

    /// Returns the taskbar configuration.
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Accesses the global icon cache.
    pub fn icon_cache(&self) -> &icon::Cache {
        &self.0.icon_cache
    }

    /// Accesses the global [`Niri`] instance.
    pub fn niri(&self) -> &Niri {
        &self.0.niri
    }

    pub async fn event_stream(&self) -> Result<impl Stream<Item = Event> + use<>, Error> {
        let (tx, rx) = async_channel::unbounded();

        if self.config().notifications_enabled() {
            glib::spawn_future_local(notify_stream(tx.clone()));
        }

        glib::spawn_future_local(window_stream(tx, self.niri().window_stream()?));

        Ok(async_stream::stream! {
            while let Ok(event) = rx.recv().await {
                yield event;
            }
        })
    }
}

#[derive(Debug)]
struct Inner {
    config: Config,
    icon_cache: icon::Cache,
    niri: Niri,
}

pub enum Event {
    Notification(Notification),
    WindowSnapshot(Snapshot),
}

async fn notify_stream(tx: Sender<Event>) {
    let mut stream = Box::pin(notify::stream());

    while let Some(notification) = stream.next().await {
        if let Err(e) = tx.send(Event::Notification(notification)).await {
            eprintln!("error sending notification: {e}");
        }
    }
}

async fn window_stream(tx: Sender<Event>, window_stream: WindowStream) {
    while let Some(snapshot) = window_stream.next().await {
        if let Err(e) = tx.send(Event::WindowSnapshot(snapshot)).await {
            eprintln!("error sending window snapshot: {e}");
        }
    }
}
