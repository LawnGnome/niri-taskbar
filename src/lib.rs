use std::collections::{BTreeMap, BTreeSet, HashMap};

use button::Button;
use config::Config;
use error::Error;
use futures::StreamExt;
use niri_ipc::Window;
use process::Process;
use state::{Event, State};
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

struct TaskbarModule {}

impl Module for TaskbarModule {
    type Config = Config;

    fn init(info: &waybar_cffi::InitInfo, config: Config) -> Self {
        let module = Self {};
        let state = State::new(config);

        let context = MainContext::default();
        if let Err(e) = context.block_on(init(info, state)) {
            eprintln!("niri taskbar module init error: {e:?}");
        }

        module
    }
}

waybar_module!(TaskbarModule);

async fn init(info: &waybar_cffi::InitInfo, state: State) -> Result<(), Error> {
    // Set up the box that we'll use to contain the actual window buttons.
    let root = info.get_root_widget();
    let container = gtk::Box::new(Orientation::Horizontal, 0);
    container.style_context().add_class("niri-taskbar");
    root.add(&container);

    // We need to spawn a task to receive the window snapshots and update the container.
    let context = MainContext::default();
    let mut stream = Box::pin(state.event_stream().await?);

    context.spawn_local(async move {
        // It's inefficient to recreate every button every time, so we keep them in a cache and
        // just reorder the container as we update based on each snapshot. This also avoids
        // flickering while rendering, since the images don't have to be reloaded from disk.
        let mut buttons: BTreeMap<u64, Button> = BTreeMap::new();

        // We need the last window snapshot to be able to detect which toplevel
        // is associated with incoming notifications.
        let mut last_snapshot: Option<Vec<Window>> = None;

        while let Some(event) = stream.next().await {
            match event {
                Event::Notification(notification) => {
                    // We'll try to set the urgent class on the relevant window
                    // if we can figure out which toplevel is associated with
                    // the notification.
                    //
                    // First, we need to see if there even is a sender PID in
                    // the notification.
                    if let Some(mut pid) = notification.hints.sender_pid {
                        if let Some(last) = &last_snapshot {
                            // The heuristic we'll use is to walk up from the
                            // sender PID and see if any of the parents are
                            // toplevels.
                            //
                            // The easiest way to do that is with a map.
                            let pids = PidWindowMap::new(last.iter());

                            loop {
                                if let Some(window) = pids.get(pid) {
                                    // If the window is already focused, there
                                    // isn't really much to do.
                                    if !window.is_focused {
                                        if let Some(button) = buttons.get(&window.id) {
                                            button.set_urgent();
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
                                        // On error, we'll log but do nothing
                                        // else: this shouldn't be fatal for the
                                        // bar.
                                        eprintln!(
                                            "error walking up from process {}: {e}",
                                            notification.hints.sender_pid.unwrap_or_default()
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Event::WindowSnapshot(windows) => {
                    // We need to track which, if any, windows are no longer
                    // present.
                    let mut omitted = buttons.keys().copied().collect::<BTreeSet<_>>();

                    for window in windows.iter() {
                        let button = buttons.entry(window.id).or_insert_with(|| {
                            let button = Button::new(&state, window);

                            // Implicitly adding the button widget to the box as
                            // we create it simplifies reordering, since it
                            // means we can just do it as we go.
                            container.add(button.widget());
                            button
                        });

                        // Update the window properties.
                        button.set_focus(window.is_focused);
                        button.set_title(window.title.as_deref());

                        // Ensure we don't remove this button from the
                        // container.
                        omitted.remove(&window.id);

                        // Since we get the windows in order in the snapshot, we
                        // can just push this to the back and then let other
                        // widgets push in front as we iterate.
                        container.reorder_child(button.widget(), -1);
                    }

                    // Remove any windows that no longer exist.
                    for id in omitted.into_iter() {
                        if let Some(button) = buttons.remove(&id) {
                            container.remove(button.widget());
                        }
                    }

                    // Ensure everything is rendered.
                    container.show_all();

                    // Update the last snapshot.
                    last_snapshot = Some(windows);
                }
            }
        }
    });

    Ok(())
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
