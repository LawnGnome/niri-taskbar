use async_channel::{Receiver, Sender};
use niri_ipc::Request;

use crate::error::Error;

use super::{
    reply, socket,
    state::{Snapshot, WindowSet},
};

/// A stream that receives events from Niri and produces a stream of window [`Snapshot`]s.
pub struct WindowStream {
    rx: Receiver<Snapshot>,
}

impl WindowStream {
    pub(super) fn new() -> Self {
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            if let Err(e) = window_stream(tx) {
                tracing::error!(%e, "Niri taskbar window stream error");
            }
        });

        Self { rx }
    }

    /// Awaits the next [`Snapshot`].
    pub async fn next(&self) -> Option<Snapshot> {
        self.rx.recv().await.ok()
    }
}

fn window_stream(tx: Sender<Snapshot>) -> Result<(), Error> {
    let (reply, mut next) = socket()?
        .send(Request::EventStream)
        .map_err(Error::NiriIpc)?;
    reply::typed!(Handled, reply)?;

    let mut state = WindowSet::new();
    loop {
        // There appears to be no EOF state, presumably on the assumption that if Niri goes away it
        // doesn't matter what happens to this process.
        match next() {
            Ok(event) => {
                if let Some(snapshot) = state.with_event(event) {
                    tx.send_blocking(snapshot)
                        .map_err(|_| Error::WindowStreamSend)?;
                }
            }
            Err(e) => {
                tracing::error!(%e, "Niri IPC error reading from event stream");
                return Err(Error::NiriIpc(e));
            }
        }
    }
}
