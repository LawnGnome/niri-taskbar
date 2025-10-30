use std::{
    collections::{BTreeMap, BTreeSet, HashMap, btree_map::Entry},
    sync::{Arc, LazyLock, Mutex},
};

use button::Button;
use config::Config;
use error::Error;
use futures::StreamExt;
use niri::{Snapshot, Window};
use notify::EnrichedNotification;
use output::Matcher;
use process::Process;
use state::{Event, State};
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};
use waybar_cffi::{
    Module,
    gtk::{
        self, Orientation, gio, glib::MainContext, prelude::Cast, traits::{ButtonExt, ContainerExt, StyleContextExt, WidgetExt}
    },
    waybar_module,
};

mod button;
mod config;
mod error;
mod icon;
mod niri;
mod notify;
mod output;
mod process;
mod state;

static TRACING: LazyLock<()> = LazyLock::new(|| {
    if let Err(e) = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_span_events(FmtSpan::CLOSE)
        .try_init()
    {
        eprintln!("cannot install global tracing subscriber: {e}");
    }
});

struct TaskbarModule {}

impl Module for TaskbarModule {
    type Config = Config;

    fn init(info: &waybar_cffi::InitInfo, config: Config) -> Self {
        // Ensure tracing-subscriber is initialised.
        *TRACING;

        let module = Self {};
        let state = State::new(config);

        let context = MainContext::default();
        if let Err(e) = context.block_on(init(info, state)) {
            tracing::error!(%e, "Niri taskbar module init failed");
        }

        module
    }
}

waybar_module!(TaskbarModule);

#[tracing::instrument(level = "DEBUG", skip_all, err)]
async fn init(info: &waybar_cffi::InitInfo, state: State) -> Result<(), Error> {
    // Set up the box that we'll use to contain the actual window buttons.
    let root = info.get_root_widget();
    let container = gtk::Box::new(Orientation::Horizontal, 0);
    container.style_context().add_class("niri-taskbar");
    root.add(&container);

    // We need to spawn a task to receive the window snapshots and update the container.
    let context = MainContext::default();
    context.spawn_local(async move { Instance::new(state, container).task().await });

    Ok(())
}

struct Instance {
    buttons: BTreeMap<u64, Button>,
    /// Map of workspace index -> label widget. Each workspace gets exactly one label
    /// shown before the first app icon belonging to that workspace.
    workspace_buttons: BTreeMap<u64, gtk::Button>,
    container: gtk::Box,
    last_snapshot: Option<Snapshot>,
    state: State,
}

impl Instance {
    pub fn new(state: State, container: gtk::Box) -> Self {
        Self {
            buttons: Default::default(),
            workspace_buttons: Default::default(),
            container,
            last_snapshot: None,
            state,
        }
    }

    pub async fn task(&mut self) {
        // We have to build the output filter here, because until the Glib event loop has run the
        // container hasn't been realised, which means we can't figure out which output we're on.
        let output_filter = Arc::new(Mutex::new(self.build_output_filter().await));

        let mut stream = match self.state.event_stream() {
            Ok(stream) => Box::pin(stream),
            Err(e) => {
                tracing::error!(%e, "error starting event stream");
                return;
            }
        };
        while let Some(event) = stream.next().await {
            match event {
                Event::Notification(notification) => self.process_notification(notification).await,
                Event::WindowSnapshot(windows) => {
                    self.process_window_snapshot(windows, output_filter.clone())
                        .await
                }
                Event::Workspaces(_) => {
                    // We're just using this as a signal that the outputs may have changed.
                    let new_filter = self.build_output_filter().await;
                    *output_filter.lock().expect("output filter lock") = new_filter;
                }
            }
        }
    }

    // Get output using the filter
    async fn get_output(&self, filter: &Arc<Mutex<output::Filter>>) -> Option<String> {
        let guard = filter.lock().ok()?;

        match &*guard {
            output::Filter::Only(output_name) => Some(output_name.clone()),
            _ => None,
        }
    }

    #[tracing::instrument(level = "DEBUG", skip(self))]
    async fn build_output_filter(&self) -> output::Filter {
        if !self.state.config().display_vars().filter_by_output {
            return output::Filter::ShowAll;
        }

        // OK, so we need to figure out what output we're on. Easy, right?
        //
        // Not so fast!
        //
        // In-tree Waybar modules have access to a Wayland client called `Client`, which they can
        // use to access the `wl_display` the bar is created against, and further access metadata
        // from there. Unfortunately, none of that is exposed in CFFI, and, honestly, I'm not really
        // sure how you would trivially wrap it in a C API.
        //
        // We have the Gtk 3 container, though, so that's something — we have to wait until the
        // window has been realised, but that's happened by the time we're in the main loop
        // callback. The problem is that we're also using Gdk 3, which doesn't expose the connection
        // name of the monitor in use, which is the only thing we can match against the Niri output
        // configuration.
        //
        // Now, this wouldn't be so bad on its own, because we _can_ get to the `wl_output` via
        // `gdkwayland`, and version 4 of the core Wayland protocol includes the output name.
        // Unfortunately, we have no way of accessing Gdk's Wayland connection, and Wayland
        // identifiers aren't stable across connections, so we can't just connect to Wayland
        // ourselves and enumerate the outputs. (Trust me, I tried.)
        //
        // So, until Waybar migrates to Gtk 4, that leaves us without a truly reliable solution.
        //
        // What we'll do instead is match up what we can. Niri can tell us everything we want to
        // know about the output, and Gdk 3 does include things like the output geometry, make, and
        // model. So we'll match on those and hope for the best.
        let niri = *self.state.niri();
        let outputs = match gio::spawn_blocking(move || niri.outputs()).await {
            Ok(Ok(outputs)) => outputs,
            Ok(Err(e)) => {
                tracing::warn!(%e, "cannot get Niri outputs");
                return output::Filter::ShowAll;
            }
            Err(_) => {
                tracing::error!("error received from gio while waiting for task");
                return output::Filter::ShowAll;
            }
        };

        // If there's only one output, then none of this matching stuff matters anyway.
        if outputs.len() == 1 {
            return output::Filter::ShowAll;
        }

        let Some(window) = self.container.window() else {
            tracing::warn!("cannot get Gdk window for container");
            return output::Filter::ShowAll;
        };

        let display = window.display();
        let Some(monitor) = display.monitor_at_window(&window) else {
            tracing::warn!(display = ?window.display(), geometry = ?window.geometry(), "cannot get monitor for window");
            return output::Filter::ShowAll;
        };

        for (name, output) in outputs.into_iter() {
            let matches = output::Matcher::new(&monitor, &output);
            if matches == Matcher::all() {
                return output::Filter::Only(name);
            }
        }

        tracing::warn!(?monitor, "no Niri output matched the Gdk monitor");
        output::Filter::ShowAll
    }

    #[tracing::instrument(level = "TRACE", skip(self))]
    async fn process_notification(&mut self, notification: Box<EnrichedNotification>) {
        // We'll try to set the urgent class on the relevant window if we can
        // figure out which toplevel is associated with the notification.
        //
        // Obviously, for that, we need toplevels.
        let Some(toplevels) = &self.last_snapshot else {
            return;
        };

        if let Some(mut pid) = notification.pid() {
            tracing::trace!(
                pid,
                "got notification with PID; trying to match it to a toplevel"
            );

            // If we have the sender PID — either from the notification itself,
            // or D-Bus — then the heuristic we'll use is to walk up from the
            // sender PID and see if any of the parents are toplevels.
            //
            // The easiest way to do that is with a map, which we can build from
            // the toplevels.
            let pids = PidWindowMap::new(toplevels.iter());

            // We'll track if we found anything, since we might fall back to
            // some fuzzy matching.
            let mut found = false;

            loop {
                if let Some(window) = pids.get(pid) {
                    // If the window is already focused, there isn't really much
                    // to do.
                    if !window.is_focused {
                        if let Some(button) = self.buttons.get(&window.id) {
                            tracing::trace!(
                                ?button,
                                ?window,
                                pid,
                                "found matching window; setting urgent"
                            );
                            button.set_urgent();
                            found = true;
                        }
                    }
                }

                match Process::new(pid).await {
                    Ok(Process { ppid }) => {
                        if let Some(ppid) = ppid {
                            // Keep walking up.
                            pid = ppid;
                        } else {
                            // There are no more parents.
                            break;
                        }
                    }
                    Err(e) => {
                        // On error, we'll log but do nothing else: this
                        // shouldn't be fatal for the bar, since it's possible
                        // the process has simply already exited.
                        tracing::info!(pid, %e, "error walking up process tree");
                        break;
                    }
                }
            }

            // If we marked one or more toplevels as urgent, then we're done.
            if found {
                return;
            }
        }

        tracing::trace!("no PID in notification, or no match found");

        // Otherwise, we'll fall back to the desktop entry if we got one, and
        // see what we can find.
        //
        // There are a bunch of things that can get in the way here.
        // Applications don't necessarily know the application ID they're
        // registered under on the system: Flatpaks, for instance, have no idea
        // what the Flatpak actually called them when installed. So we'll do our
        // best and make some educated guesses, but that's really what it is.
        if !self.state.config().notifications_use_desktop_entry() {
            tracing::trace!("use of desktop entries is disabled; no match found");
            return;
        }
        let Some(desktop_entry) = &notification.notification().hints.desktop_entry else {
            tracing::trace!("no desktop entry found in notification; nothing more to be done");
            return;
        };

        // So we only have to walk the window list once, we'll keep track of the
        // fuzzy matches we find, even if we don't use them.
        let use_fuzzy = self.state.config().notifications_use_fuzzy_matching();
        let mut fuzzy = Vec::new();

        // XXX: do we still need this with fuzzy matching?
        let mapped = self
            .state
            .config()
            .notifications_app_map(desktop_entry)
            .unwrap_or(desktop_entry);
        let mapped_lower = mapped.to_lowercase();
        let mapped_last_lower = mapped
            .split('.')
            .next_back()
            .unwrap_or_default()
            .to_lowercase();

        let mut found = false;
        for window in toplevels.iter() {
            let Some(app_id) = window.app_id.as_deref() else {
                continue;
            };

            if app_id == mapped {
                if let Some(button) = self.buttons.get(&window.id) {
                    tracing::trace!(app_id, ?button, ?window, "toplevel match found via app ID");
                    button.set_urgent();
                    found = true;
                }
            } else if use_fuzzy {
                // See if we have a fuzzy match, which we'll basically specify
                // as "does the app ID match case insensitively, or does the
                // last component of the app ID match the last component of the
                // desktop entry?".
                if app_id.to_lowercase() == mapped_lower {
                    tracing::trace!(
                        app_id,
                        ?window,
                        "toplevel match found via case-transformed app ID"
                    );
                    fuzzy.push(window.id);
                } else if app_id.contains('.') {
                    tracing::trace!(
                        app_id,
                        ?window,
                        "toplevel match found via last element of app ID"
                    );
                    if let Some(last) = app_id.split('.').next_back() {
                        if last.to_lowercase() == mapped_last_lower {
                            fuzzy.push(window.id);
                        }
                    }
                }
            }
        }

        if !found {
            for id in fuzzy.into_iter() {
                if let Some(button) = self.buttons.get(&id) {
                    button.set_urgent();
                }
            }
        }
    }

    #[tracing::instrument(level = "DEBUG", skip(self))]
    async fn process_window_snapshot(
        &mut self,
        windows: Snapshot,
        filter: Arc<Mutex<output::Filter>>,
    ) {
        // We need to track which, if any, windows are no longer present.
        let mut omitted = self.buttons.keys().copied().collect::<BTreeSet<_>>();

        // We'll track which workspaces we saw in this snapshot so we can remove labels for
        // workspaces that no longer have any windows.
        let mut seen_workspaces: BTreeSet<u64> = Default::default();

        // Collect the windows we should show according to the output filter so we can both
        // create/update widgets and then deterministically rebuild the container order.
        let mut filtered_windows: Vec<_> = windows
            .iter()
            .filter(|window| {
                filter
                    .lock()
                    .expect("output filter lock")
                    .should_show(window.output().unwrap_or_default())
            })
            .collect();

        // Filter windows by workspace only works if theyre also filtered by output
        // thats fine though because of DisplayVars enum. Stil be carefull
        // This could be changed not necessary for now
        if self.state.config().display_vars().filter_by_workspace {

            let active_workspace_idx = match self.get_output(&filter).await {
                Some(output) => self.state.niri().get_active_workspace_index_output(&output),
                _ => None,
            };

            if let Some(idx) = active_workspace_idx {
                filtered_windows.retain(|window| {
                    window.workspace_idx()  == idx as u64
                });
            }

        }
        for window in filtered_windows.iter().copied() {
            seen_workspaces.insert(window.workspace_idx());

            // If configured, ensure a label exists for this workspace even if the
            // button already existed; this prevents labels from disappearing when
            // windows move between workspaces.
            let ws_idx = window.workspace_idx();
            if self.state.config().display_vars().workspace_buttons {
                if !self.workspace_buttons.contains_key(&ws_idx) {
                    let button = gtk::Button::with_label(&ws_idx.to_string());
                    button.style_context().add_class("taskbar-button-workspace");


                    let statec = self.state.clone();
                    button.connect_clicked(move |_| {
                        if let Err(e) = statec.niri().activate_workspace(ws_idx as u8) {
                            tracing::warn!(%e, id = ws_idx, "error trying to activate workspace");
                        }
                    });

                    self.container.add(&button);
                    self.workspace_buttons.insert(ws_idx, button);
                }
            }

            let button = match self.buttons.entry(window.id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let button = Button::new(&self.state, window);

                    // We add the widget here; container ordering will be rebuilt below so the
                    // precise position doesn't matter yet.
                    self.container.add(button.widget());
                    button.widget().style_context().add_class("taskbar-button-window");
                    entry.insert(button)
                }
            };

            // Update the window properties.
            button.set_focus(window.is_focused);
            button.set_title(window.title.as_deref());

            // Ensure we don't remove this button from the container.
            omitted.remove(&window.id);

            // No per-button reordering here: we'll rebuild the container order below to
            // ensure each workspace label appears immediately before its first app icon.
        }

        // Remove any windows that no longer exist.
        for id in omitted.into_iter() {
            if let Some(button) = self.buttons.remove(&id) {
                self.container.remove(button.widget());
            }
        }

        // Remove any workspace labels for workspaces we didn't see.
        if self.state.config().display_vars().workspace_buttons {
            let existing_ws: Vec<u64> = self.workspace_buttons.keys().copied().collect();
            for ws in existing_ws.into_iter() {
                if !seen_workspaces.contains(&ws) {
                    if let Some(button) = self.workspace_buttons.remove(&ws) {
                        self.container.remove(&button);
                    }
                }
            }
        } else {
            // If workspace numbers are disabled, ensure any existing labels are removed
            // from the container and cleared.
            if !self.workspace_buttons.is_empty() {
                // Consume the map so we can remove widgets from the container.
                let buttons = std::mem::take(&mut self.workspace_buttons);
                for (_ws, button) in buttons.into_iter() {
                    self.container.remove(&button);
                }
            }
        }

        // Loop over Workspace Buttons and set focused only works with output Filter
        // see: workspace filtering
        if let Some(output) = self.get_output(&filter).await {

            for button_t in &self.workspace_buttons {

                let (idx, button) = button_t;

                let context = &button.style_context();

                if let Some(active_workspace) = self.state.niri().get_active_workspace_index_output(&output){
                    if *idx == active_workspace as u64 {
                        context.add_class("focused");
                    } else {
                        context.remove_class("focused");
                    }
                }
            }
        }

        // Rebuild the container order so that each workspace label appears immediately
        // before the first app icon that belongs to that workspace.
        let mut desired: Vec<gtk::Widget> = Vec::new();
        let mut pushed_ws: BTreeSet<u64> = Default::default();

        for window in filtered_windows.iter().copied() {
            let ws_idx = window.workspace_idx();
            if !pushed_ws.contains(&ws_idx) {
                if self.state.config().display_vars().workspace_buttons {
                    if let Some(button) = self.workspace_buttons.get(&ws_idx) {
                        desired.push(button.clone().upcast::<gtk::Widget>());
                        pushed_ws.insert(ws_idx);
                    }
                }
            }

            if let Some(button) = self.buttons.get(&window.id) {
                desired.push(button.widget().clone().upcast::<gtk::Widget>());
            }
        }

        // Remove all existing children and re-add in the desired order.
        for child in self.container.children() {
            self.container.remove(&child);
        }

        for widget in desired.into_iter() {
            self.container.add(&widget);
        }

        // Ensure everything is rendered.
        self.container.show_all();

        // Update the last snapshot.
        self.last_snapshot = Some(windows);
    }

}

/// A basic map of PIDs to windows.
///
/// Windows that don't have a PID are ignored, since we can't match on them
/// anyway. (Also, how does that happen?)
struct PidWindowMap<'a>(HashMap<i64, &'a Window>);

impl<'a> PidWindowMap<'a> {
    fn new(iter: impl Iterator<Item = &'a Window>) -> Self {
        Self(
            iter.filter_map(|window| window.pid.map(|pid| (i64::from(pid), window)))
                .collect(),
        )
    }

    fn get(&self, pid: i64) -> Option<&'a Window> {
        self.0.get(&pid).copied()
    }
}
