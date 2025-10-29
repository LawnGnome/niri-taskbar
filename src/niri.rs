use std::collections::HashMap;

use futures::Stream;
use niri_ipc::{Response, Action, Event, Output, Reply, Request, Workspace, socket::Socket, WorkspaceReferenceArg};
pub use state::{Snapshot, Window};
pub use window_stream::WindowStream;

use crate::error::Error;

mod reply;
mod state;
mod window_stream;

/// The top level client for Niri.
#[derive(Debug, Clone, Copy)]
pub struct Niri {}

impl Niri {
    pub fn new() -> Self {
        // Since niri_ipc is essentially stateless, we don't maintain anything much here.
        Self {}
    }

    /// Requests that the given window ID should be activated.
    #[tracing::instrument(level = "TRACE", err)]
    pub fn activate_window(&self, id: u64) -> Result<(), Error> {
        let reply = request(Request::Action(Action::FocusWindow { id }))?;
        reply::typed!(Handled, reply)
    }

    /// Requests that the given workspace index should be activated.
    pub fn activate_workspace(&self, idx: u8) -> Result<(), Error> {
        let wsr = WorkspaceReferenceArg::Index(idx);
        let reply = request(Request::Action(Action::FocusWorkspace { reference: wsr}))?;
        reply::typed!(Handled, reply)
    }

    /// Returns the current outputs.
    pub fn outputs(&self) -> Result<HashMap<String, Output>, Error> {
        let reply = request(Request::Outputs)?;
        reply::typed!(Outputs, reply)
    }

    /// Returns a stream of window snapshots.
    pub fn window_stream(&self) -> WindowStream {
        WindowStream::new()
    }

    /// Returns index of active workspace given output
    pub fn get_active_workspace_index_output(&self, output: &str) -> Option<u8> {
        let reply = request(Request::Workspaces).ok()?;
        if let Ok(response) = reply {
            if let Response::Workspaces(list_w) = response {
                for workspace in list_w {
                    if workspace.is_active && workspace.output? == output {
                        return Some(workspace.idx)
                    }
                }
            } 
        }
        None
    }

    /// Returns a stream of workspace changes.
    pub fn workspace_stream(&self) -> Result<impl Stream<Item = Vec<Workspace>> + use<>, Error> {
        let mut socket = socket()?;
        let reply = socket.send(Request::EventStream).map_err(Error::NiriIpc)?;
        reply::typed!(Handled, reply)?;

        let mut next = socket.read_events();
        Ok(async_stream::stream! {
            loop {
                match next() {
                    Ok(Event::WorkspacesChanged { workspaces }) => {
                        yield workspaces;
                    }
                    Ok(_) => (),
                    Err(e) => {
                        tracing::error!(%e, "Niri IPC error reading from event stream");
                    }
                }
            }
        })
    }
}

// Helper to marshal request errors into our own type system.
//
// This can't be used for event streams, since the stream callback is thrown away in this function.
#[tracing::instrument(level = "TRACE", err)]
fn request(request: Request) -> Result<Reply, Error> {
    socket()?.send(request).map_err(Error::NiriIpc)
}

// Helper to connect to the Niri socket.
#[tracing::instrument(level = "TRACE", err)]
fn socket() -> Result<Socket, Error> {
    Socket::connect().map_err(Error::NiriIpc)
}
