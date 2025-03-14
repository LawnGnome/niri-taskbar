use std::{cmp::Ordering, collections::BTreeMap, fmt::Display};

use niri_ipc::{Event, Window, Workspace};

/// The toplevel window set within Niri, updated via the Niri event stream.
#[derive(Debug)]
pub struct WindowSet(Option<Inner>);

impl WindowSet {
    /// Creates a new window set.
    pub fn new() -> Self {
        Self(None)
    }

    /// Updates the window set based on the given [`niri_ipc::Event`].
    #[tracing::instrument(level = "TRACE", skip(self))]
    pub fn with_event(&mut self, event: Event) -> Option<Snapshot> {
        // This is mildly annoying, because Niri actually has the same state within it and could
        // easily send it on each event, but we have to replicate Niri's own logic and hope we get
        // it right.
        match event {
            Event::WindowsChanged { windows } => match self.0.take() {
                Some(Inner::WorkspacesOnly(workspaces)) => {
                    self.0 = Some(Inner::Ready(Niri::new(windows, workspaces)));
                }
                Some(Inner::WindowsOnly(_)) | None => {
                    self.0 = Some(Inner::WindowsOnly(windows));
                }
                Some(Inner::Ready(mut state)) => {
                    state.replace_windows(windows);
                    self.0 = Some(Inner::Ready(state));
                }
            },
            Event::WorkspacesChanged { workspaces } => match self.0.take() {
                Some(Inner::WindowsOnly(windows)) => {
                    self.0 = Some(Inner::Ready(Niri::new(windows, workspaces)));
                }
                Some(Inner::WorkspacesOnly(_)) | None => {
                    self.0 = Some(Inner::WorkspacesOnly(workspaces));
                }
                Some(Inner::Ready(mut state)) => {
                    state.replace_workspaces(workspaces);
                    self.0 = Some(Inner::Ready(state));
                }
            },
            Event::WindowClosed { id } => {
                if let Some(Inner::Ready(state)) = &mut self.0 {
                    state.remove_window(id);
                } else {
                    tracing::warn!(%self, "unexpected state for WindowClosed event");
                }
            }
            Event::WindowOpenedOrChanged { window } => {
                if let Some(Inner::Ready(state)) = &mut self.0 {
                    state.upsert_window(window);
                } else {
                    tracing::warn!(%self, "unexpected state for WindowOpenedOrChanged event");
                }
            }
            Event::WindowFocusChanged { id } => {
                if let Some(Inner::Ready(state)) = &mut self.0 {
                    state.set_focus(id);
                } else {
                    tracing::warn!(%self, "unexpected state for WindowFocusChanged event");
                }
            }
            _ => {}
        }

        if let Some(Inner::Ready(state)) = &self.0 {
            Some(state.snapshot())
        } else {
            None
        }
    }
}

impl Display for WindowSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match &self.0 {
                Some(Inner::Ready(_)) => "ready",
                Some(Inner::WindowsOnly(_)) => "windows only",
                Some(Inner::WorkspacesOnly(_)) => "workspaces only",
                None => "uninitialised",
            }
        )
    }
}

/// The inner state machine as we establish a new event stream.
///
/// Niri guarantees that we will get [`niri_ipc::Event::WindowsChanged`] and
/// [`niri_ipc::Event::WorkspacesChanged`] events at the start of the stream before getting any
/// update events, but not which order they'll come in, so we have to handle that as we build up
/// the window set.
#[derive(Debug)]
enum Inner {
    WindowsOnly(Vec<Window>),
    WorkspacesOnly(Vec<Workspace>),
    Ready(Niri),
}

/// The Niri state, as best as we can reconstruct it based on the event stream.
#[derive(Debug)]
struct Niri {
    windows: BTreeMap<u64, Window>,
    workspaces: BTreeMap<u64, Workspace>,
}

impl Niri {
    fn new(windows: Vec<Window>, workspaces: Vec<Workspace>) -> Self {
        let mut niri = Niri {
            windows: Default::default(),
            workspaces: Default::default(),
        };

        niri.replace_workspaces(workspaces);
        niri.replace_windows(windows);

        niri
    }

    fn remove_window(&mut self, id: u64) {
        self.windows.remove(&id);
    }

    fn replace_windows(&mut self, windows: Vec<Window>) {
        self.windows = windows
            .into_iter()
            .map(|window| (window.id, window))
            .collect();
    }

    fn replace_workspaces(&mut self, workspaces: Vec<Workspace>) {
        self.workspaces = workspaces.into_iter().map(|ws| (ws.id, ws)).collect();
    }

    fn set_focus(&mut self, id: Option<u64>) {
        // We have to manually patch up the window is_focused values.
        for window in self.windows.values_mut() {
            window.is_focused = Some(window.id) == id;
        }
    }

    fn upsert_window(&mut self, window: Window) {
        // Ensure that we update other windows if the new window is focused.
        if window.is_focused {
            self.windows.values_mut().for_each(|window| {
                window.is_focused = false;
            })
        }

        self.windows.insert(window.id, window);
    }

    /// Create a snapshot of the current window state, ordered by workspace index.
    fn snapshot(&self) -> Snapshot {
        struct WindowWorkspace<'a> {
            window: &'a Window,
            workspace: &'a Workspace,
        }

        let mut wws: Vec<_> = self
            .windows
            .values()
            .filter_map(|window| {
                if let Some(ws_id) = window.workspace_id {
                    if let Some(workspace) = self.workspaces.get(&ws_id) {
                        return Some(WindowWorkspace { window, workspace });
                    }
                }
                None
            })
            .collect();
        wws.sort_by(|a, b| match a.workspace.idx.cmp(&b.workspace.idx) {
            // Once https://github.com/YaLTeR/niri/discussions/721 is resolved, we can order
            // windows within a workspace based on their origin, which should feel more natural.
            Ordering::Equal => a.window.id.cmp(&b.window.id),
            order => order,
        });

        wws.into_iter().map(|ww| ww.window.clone()).collect()
    }
}

/// A snapshot of current toplevel windows, ordered by workspace index.
pub type Snapshot = Vec<Window>;
