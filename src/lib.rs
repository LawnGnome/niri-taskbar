use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::LazyLock,
};

use button::Button;
use config::Config;
use error::Error;
use futures::StreamExt;
use niri_ipc::Window;
use notify::EnrichedNotification;
use process::Process;
use state::{Event, State};
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};
use waybar_cffi::{
    Module,
    gtk::{
        self, Orientation,
        glib::MainContext,
        traits::{BoxExt, ContainerExt, StyleContextExt, WidgetExt},
    },
    waybar_module,
};

mod button;
mod config;
mod error;
mod icon;
mod niri;
mod notify;
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
    container: gtk::Box,
    last_snapshot: Option<Vec<Window>>,
    state: State,
}

impl Instance {
    pub fn new(state: State, container: gtk::Box) -> Self {
        Self {
            buttons: Default::default(),
            container,
            last_snapshot: None,
            state,
        }
    }

    pub async fn task(&mut self) {
        let mut stream = Box::pin(self.state.event_stream().await);

        while let Some(event) = stream.next().await {
            match event {
                Event::Notification(notification) => self.process_notification(notification).await,
                Event::WindowSnapshot(windows) => self.process_window_snapshot(windows).await,
            }
        }
    }

    #[tracing::instrument(level = "DEBUG", skip(self))]
    async fn process_notification(&mut self, notification: EnrichedNotification) {
        // We'll try to set the urgent class on the relevant window if we can
        // figure out which toplevel is associated with the notification.
        //
        // Obviously, for that, we need toplevels.
        let Some(toplevels) = &self.last_snapshot else {
            return;
        };

        if let Some(mut pid) = notification.pid() {
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

        // Otherwise, we'll fall back to the desktop entry if we got one, and
        // see what we can find.
        //
        // There are a bunch of things that can get in the way here.
        // Applications don't necessarily know the application ID they're
        // registered under on the system: Flatpaks, for instance, have no idea
        // what the Flatpak actually called them when installed. So we'll do our
        // best and make some educated guesses, but that's really what it is.
        if !self.state.config().notifications_use_desktop_entry() {
            return;
        }
        let Some(desktop_entry) = &notification.notification().hints.desktop_entry else {
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
            .notifications_app_map(&desktop_entry)
            .unwrap_or(desktop_entry);
        let mapped_lower = mapped.to_lowercase();
        let mapped_last_lower = mapped.split('.').last().unwrap_or_default().to_lowercase();

        let mut found = false;
        for window in toplevels.iter() {
            let Some(app_id) = window.app_id.as_deref() else {
                continue;
            };

            if app_id == mapped {
                if let Some(button) = self.buttons.get(&window.id) {
                    button.set_urgent();
                    found = true;
                }
            } else if use_fuzzy {
                // See if we have a fuzzy match, which we'll basically specify
                // as "does the app ID match case insensitively, or does the
                // last component of the app ID match the last component of the
                // desktop entry?".
                if app_id.to_lowercase() == mapped_lower {
                    fuzzy.push(window.id);
                } else if app_id.contains('.') {
                    if let Some(last) = app_id.split('.').last() {
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
    async fn process_window_snapshot(&mut self, windows: Vec<Window>) {
        // We need to track which, if any, windows are no longer present.
        let mut omitted = self.buttons.keys().copied().collect::<BTreeSet<_>>();

        for window in windows.iter() {
            let button = self.buttons.entry(window.id).or_insert_with(|| {
                let button = Button::new(&self.state, window);

                // Implicitly adding the button widget to the box as we create
                // it simplifies reordering, since it means we can just do it as
                // we go.
                self.container.add(button.widget());
                button
            });

            // Update the window properties.
            button.set_focus(window.is_focused);
            button.set_title(window.title.as_deref());

            // Ensure we don't remove this button from the container.
            omitted.remove(&window.id);

            // Since we get the windows in order in the snapshot, we can just
            // push this to the back and then let other widgets push in front as
            // we iterate.
            self.container.reorder_child(button.widget(), -1);
        }

        // Remove any windows that no longer exist.
        for id in omitted.into_iter() {
            if let Some(button) = self.buttons.remove(&id) {
                self.container.remove(button.widget());
            }
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
