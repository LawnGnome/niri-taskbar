use std::{ops::Deref, time::Duration};

use async_channel::Sender;
use futures::{Stream, TryStreamExt};
use itertools::Itertools;
use serde::{Deserialize, Deserializer};
use waybar_cffi::gtk::glib::{self};
use zbus::{
    Connection, MatchRule, MessageStream,
    fdo::MonitoringProxy,
    names::{InterfaceName, MemberName},
    zvariant::{DeserializeDict, Optional, Type},
};

mod cache;

/// Starts a stream of notifications.
///
/// Under the hood, this sets up a monitor on the D-Bus session bus and grabs
/// any method call to the `Notify` method on the
/// `org.freedesktop.Notifications` interface.
pub fn stream() -> impl Stream<Item = EnrichedNotification> {
    // For lifetime reasons, it's easier to have an async channel extract the
    // data out of the GLib event loop than it is to return the stream directly.
    let (tx, rx) = async_channel::unbounded();
    glib::spawn_future_local(async move {
        match monitor_dbus(tx).await {
            Ok(()) => tracing::info!("no longer monitoring D-Bus"),
            Err(e) => tracing::error!(%e, "D-Bus error"),
        }
    });

    async_stream::stream! {
        while let Ok(notification) = rx.recv().await {
            yield notification;
        }
    }
}

/// A FDO notification with the PID of the connection that sent it, if
/// available.
#[derive(Debug, Clone)]
pub struct EnrichedNotification {
    notification: Notification,
    pid: Option<u32>,
}

impl EnrichedNotification {
    /// Returns a reference to the notification.
    pub fn notification(&self) -> &Notification {
        &self.notification
    }

    /// Returns the PID, either from the connection or the `sender-pid`
    /// notification hint.
    pub fn pid(&self) -> Option<i64> {
        match self.pid {
            Some(pid) => Some(pid.into()),
            None => self.notification.hints.sender_pid,
        }
    }
}

/// A FDO notification.
//
// We're parsing out more than we need here, but I'm hoping this'll be useful
// elsewhere later.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Type)]
pub struct Notification {
    pub app_name: Optional<String>,
    pub replaces_id: Optional<u32>,
    pub app_icon: Optional<String>,
    pub summary: String,
    pub body: Optional<String>,
    pub actions: Actions,
    pub hints: Hints,
    pub expire_timeout: i32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Type)]
#[zvariant(signature = "as")]
pub struct Actions(Vec<Action>);

impl Deref for Actions {
    type Target = Vec<Action>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Action {
    pub id: String,
    pub localised: String,
}

impl<'de> Deserialize<'de> for Actions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(
            Vec::<String>::deserialize(deserializer)?
                .into_iter()
                .tuples::<(_, _)>()
                .map(|(id, localised)| Action { id, localised })
                .collect(),
        ))
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, DeserializeDict, Type)]
#[zvariant(rename_all = "kebab-case", signature = "a{sv}")]
pub struct Hints {
    pub action_icons: Option<bool>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    pub resident: Option<bool>,
    pub sound_file: Option<String>,
    pub sound_name: Option<String>,
    pub suppress_sound: Option<bool>,
    pub transient: Option<bool>,
    pub sender_pid: Option<i64>,
    pub urgency: Option<u8>,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

static INTERFACE: &str = "org.freedesktop.Notifications";
static METHOD: &str = "Notify";

#[tracing::instrument(level = "TRACE", skip_all, err)]
async fn monitor_dbus(tx: Sender<EnrichedNotification>) -> anyhow::Result<()> {
    let cache = cache::ConnectionCache::new(Duration::from_secs(86400));

    let conn = Connection::session().await?;
    let proxy = MonitoringProxy::new(&conn).await?;
    proxy
        .become_monitor(
            &[MatchRule::builder()
                .interface(INTERFACE)?
                .member(METHOD)?
                .build()],
            0,
        )
        .await?;

    let mut stream = MessageStream::from(conn);
    while let Some(msg) = stream.try_next().await? {
        if msg.header().interface() == Some(&InterfaceName::from_static_str(INTERFACE)?)
            && msg.header().member() == Some(&MemberName::from_static_str(METHOD)?)
        {
            // Pull the PID out of the connection cache, if we can.
            //
            // This isn't always useful: anything in a Flatpak is going to use
            // the portal's connection, which won't map to a toplevel, but it's
            // better than nothing.
            let pid = if let Some(sender) = msg.header().sender() {
                cache.get(sender).await
            } else {
                None
            };

            tx.send(EnrichedNotification {
                notification: msg.body().deserialize()?,
                pid,
            })
            .await?;
        }
    }

    Ok(())
}
