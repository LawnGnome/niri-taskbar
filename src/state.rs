use std::sync::Arc;

use async_channel::Sender;
use futures::{Stream, StreamExt};
use niri_ipc::Workspace;
use waybar_cffi::gtk::glib;

use crate::{
    config::Config,
    error::Error,
    icon,
    niri::{Niri, Snapshot, WindowStream},
    notify::{self, EnrichedNotification},
};

/// Global state for the taskbar.
#[derive(Debug, Clone)]
pub struct State(Arc<Inner>);

impl State {
    /// Instantiates the global state.
    pub fn new(config: Config) -> Self {
        Self(Arc::new(Inner {
            config: config.clone(),
            icon_cache: icon::Cache::default(),
            niri: Niri::new(config),
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

    pub fn event_stream(&self) -> Result<impl Stream<Item = Event> + use<>, Error> {
        let (tx, rx) = async_channel::unbounded();

        if self.config().notifications_enabled() {
            glib::spawn_future_local(notify_stream(tx.clone()));
        }

        glib::spawn_future_local(window_stream(tx.clone(), self.niri().window_stream()));

        // We don't want to send a set of workspaces through until after the window stream has
        // yielded a window snapshot, and it's easier to defer it here than in the calling code.
        let mut delay = Some((tx, self.niri().workspace_stream()?));

        Ok(async_stream::stream! {
            while let Ok(event) = rx.recv().await {
                if let Some((tx, stream)) = delay.take() {
                    if let &Event::Workspaces(_) = &event {
                        glib::spawn_future_local(workspace_stream(tx, stream));
                    }
                }

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
    Notification(Box<EnrichedNotification>),
    WindowSnapshot(Snapshot),
    Workspaces(()),
}

async fn notify_stream(tx: Sender<Event>) {
    let mut stream = Box::pin(notify::stream());

    while let Some(notification) = stream.next().await {
        if let Err(e) = tx.send(Event::Notification(Box::new(notification))).await {
            tracing::error!(%e, "error sending notification");
        }
    }
}

async fn window_stream(tx: Sender<Event>, window_stream: WindowStream) {
    while let Some(snapshot) = window_stream.next().await {
        if let Err(e) = tx.send(Event::WindowSnapshot(snapshot)).await {
            tracing::error!(%e, "error sending window snapshot");
        }
    }
}

async fn workspace_stream(tx: Sender<Event>, workspace_stream: impl Stream<Item = Vec<Workspace>>) {
    let mut workspace_stream = Box::pin(workspace_stream);
    while workspace_stream.next().await.is_some() {
        if let Err(e) = tx.send(Event::Workspaces(())).await {
            tracing::error!(%e, "error sending workspaces");
        }
    }
}
