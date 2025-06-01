use niri_ipc::{Action, Reply, Request, socket::Socket};
use window_stream::WindowStream;

use crate::{config::Config, error::Error};

mod reply;
mod state;
mod window_stream;

/// The top level client for Niri.
#[derive(Debug, Clone)]
pub struct Niri {
    config: Config,
}

impl Niri {
    pub fn new(config: Config) -> Self {
        // Since niri_ipc is essentially stateless, we don't maintain anything much here.
        Self { config }
    }

    /// Requests that the given window ID should be activated.
    pub fn activate_window(&self, id: u64) -> Result<(), Error> {
        let reply = request(Request::Action(Action::FocusWindow { id }))?;
        reply::typed!(Handled, reply)
    }

    /// Returns a stream of window snapshots.
    pub fn window_stream(&self) -> Result<WindowStream, Error> {
        WindowStream::new(self.config.only_current_workspace())
    }
}

// Helper to marshal request errors into our own type system.
//
// This can't be used for event streams, since the stream callback is thrown away in this function.
fn request(request: Request) -> Result<Reply, Error> {
    socket()?.send(request).map_err(Error::NiriIpc)
}

// Helper to connect to the Niri socket.
fn socket() -> Result<Socket, Error> {
    Socket::connect().map_err(Error::NiriIpc)
}
