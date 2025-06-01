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
    pub(super) fn new() -> Result<Self, Error> {
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || {
            if let Err(e) = window_stream(tx) {
                eprintln!("niri taskbar window stream error: {e:?}");
            }
        });

        Ok(Self { rx })
    }

    /// Awaits the next [`Snapshot`].
    pub async fn next(&self) -> Option<Snapshot> {
        self.rx.recv().await.ok()
    }
}

fn window_stream(tx: Sender<Snapshot>) -> Result<(), Error> {
    let mut socket = socket()?;
    let reply = socket.send(Request::EventStream).map_err(Error::NiriIpc)?;
    reply::typed!(Handled, reply)?;
    let mut next = socket.read_events();

    // XXX: it's not clear to me if there are error conditions that make sense
    // to handle besides EOF, but it's also not clear that there's actually a
    // way to detect just that.
    let mut state = WindowSet::new();
    while let Ok(event) = next() {
        if let Some(snapshot) = state.with_event(event) {
            tx.send_blocking(snapshot)
                .map_err(|_| Error::WindowStreamSend)?;
        }
    }

    Ok(())
}
