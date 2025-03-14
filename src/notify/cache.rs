//! Basic D-Bus connection->PID cache functionality.

use std::{
    collections::HashMap,
    fmt::Debug,
    time::{Duration, SystemTime},
};

use async_channel::{Receiver, Sender};
use futures::{FutureExt, StreamExt, TryStreamExt, channel::oneshot};
use waybar_cffi::gtk::glib;
use zbus::{
    Connection, MatchRule, MessageStream,
    fdo::{DBusProxy, MonitoringProxy, NameOwnerChanged},
    message::Type,
    names::UniqueName,
};

/// A basic cache that maps D-Bus connections to PIDs.
#[derive(Debug, Clone)]
pub struct ConnectionCache {
    tx: Sender<Request>,
}

impl ConnectionCache {
    /// Instantiates a new cache.
    ///
    /// The expiry is best effort. Values below 5 minutes are unlikely to be
    /// very effective.
    pub fn new(expiry: Duration) -> Self {
        let (tx, rx) = async_channel::unbounded();
        glib::spawn_future_local(async move {
            if let Err(e) = worker(rx, expiry).await {
                tracing::error!(%e, "connection cache worker error");
            }
        });

        Self { tx }
    }

    /// Returns the PID for the given connection, if known.
    ///
    /// The D-Bus server will be asked for the PID if it is not already in the
    /// cache.
    #[tracing::instrument(level = "TRACE", skip(self))]
    pub async fn get(&self, connection: impl ToString + Debug) -> Option<u32> {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self
            .tx
            .send(Request::Get {
                connection: connection.to_string(),
                result: tx,
            })
            .await
        {
            tracing::error!(%e, "error sending request to connection cache");
            return None;
        }

        rx.await.unwrap_or(None)
    }
}

#[derive(Debug)]
enum Request {
    Get {
        connection: String,
        result: oneshot::Sender<Option<u32>>,
    },
}

#[derive(Debug)]
struct Entry {
    pid: Option<u32>,
    expiry: SystemTime,
}

static DBUS_INTERFACE: &str = "org.freedesktop.DBus";

async fn worker(rx: Receiver<Request>, expiry: Duration) -> anyhow::Result<()> {
    // The actual cache implementation here is extremely straightforward: we'll
    // maintain a HashMap on this task that we add to as we see new connections
    // to D-Bus, and also as we get requests for D-Bus connections that may
    // predate the taskbar starting up.
    //
    // We expire connections every minute. (This may be too aggressive, but it's
    // a reasonable starting point.) Each time a connection is looked up, the
    // expiry resets.
    //
    // We'll also remove connections if we get notified by D-Bus that they are
    // no longer in use.
    let mut cache = Cache::new(expiry);

    let dbus_conn = Connection::session().await?;
    let dbus_proxy = DBusProxy::new(&dbus_conn).await?;

    let monitor_conn = Connection::session().await?;
    let monitor_proxy = MonitoringProxy::new(&monitor_conn).await?;
    monitor_proxy
        .become_monitor(
            &[MatchRule::builder()
                .msg_type(Type::Signal)
                .interface(DBUS_INTERFACE)?
                .member("NameOwnerChanged")?
                .build()],
            0,
        )
        .await?;

    let mut cleanup = glib::interval_stream(Duration::from_secs(60)).fuse();

    let mut stream = MessageStream::from(monitor_conn);
    loop {
        // I don't love this select!: ideally, I'd like to move more of this out
        // of a macro that mostly breaks rust-analyzer, but since we have to
        // control the actual behaviour of the loop, this is probably the
        // least-worst solution right now.
        futures::select! {
            result = stream.try_next() => {
                match result {
                    Ok(Some(msg)) => {
                        handle_zbus_message(&mut cache, &dbus_proxy, msg).await;
                    }
                    Ok(None) => {
                        // Stream closed; error and return.
                        tracing::error!("D-Bus monitor stream closed unexpectedly");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(%e, "D-Bus monitor stream error");
                        anyhow::bail!(e);
                    }
                }
            }
            result = rx.recv().fuse() => {
                match result {
                    Ok(msg) => {
                        handle_message(&mut cache, &dbus_proxy, msg).await;
                    }
                    Err(_) => {
                        // If the channel is closed, we can't receive any more
                        // requests, so the cache is no longer needed.
                        break;
                    }
                }
            }
            _ = cleanup.next() => {
                cache.expire(SystemTime::now());
            }
        }
    }

    Ok(())
}

async fn handle_zbus_message<'a>(
    cache: &mut Cache,
    dbus_proxy: &DBusProxy<'a>,
    message: zbus::Message,
) {
    if let Some(message) = NameOwnerChanged::from_message(message) {
        if let Ok(args) = message.args() {
            if let Some(new_owner) = args.new_owner().as_ref() {
                if let Ok(pid) = dbus_proxy
                    .get_connection_unix_process_id(new_owner.clone().into())
                    .await
                {
                    cache.insert(new_owner, Some(pid));
                }
            } else if let Some(old_owner) = args.old_owner.as_ref() {
                cache.remove(old_owner);
            }
        }
    }
}

async fn handle_message<'a>(cache: &mut Cache, dbus_proxy: &DBusProxy<'a>, message: Request) {
    match message {
        Request::Get { connection, result } => {
            if let Some(maybe_pid) = cache.get(&connection) {
                let _ = result.send(maybe_pid);
            } else if let Ok(name) = UniqueName::try_from(connection.as_str()) {
                if let Ok(pid) = dbus_proxy.get_connection_unix_process_id(name.into()).await {
                    cache.insert(connection, Some(pid));
                    let _ = result.send(Some(pid));
                }
            }
        }
    }
}

#[derive(Debug)]
struct Cache {
    cache: HashMap<String, Entry>,
    expiry: Duration,
}

impl Cache {
    pub fn new(expiry: Duration) -> Self {
        Self {
            cache: Default::default(),
            expiry,
        }
    }

    pub fn expire(&mut self, now: SystemTime) {
        self.cache.retain(|_, entry| entry.expiry > now);
    }

    pub fn get(&mut self, connection: &str) -> Option<Option<u32>> {
        self.cache.get_mut(connection).map(|entry| {
            entry.expiry = SystemTime::now() + self.expiry;
            entry.pid
        })
    }

    pub fn insert(&mut self, connection: impl ToString, pid: Option<u32>) {
        self.cache.insert(
            connection.to_string(),
            Entry {
                pid,
                expiry: SystemTime::now() + self.expiry,
            },
        );
    }

    pub fn remove(&mut self, connection: &str) {
        self.cache.remove(connection);
    }
}
