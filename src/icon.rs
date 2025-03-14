use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};

use waybar_cffi::gtk::{
    gio::DesktopAppInfo,
    prelude::{AppInfoExt, IconExt},
};

/// A cache for taskbar icons.
#[derive(Debug, Clone, Default)]
pub struct Cache(Arc<Mutex<HashMap<String, PathBuf>>>);

impl Cache {
    /// Look up an icon for the given application ID.
    #[tracing::instrument(level = "TRACE", ret)]
    pub fn lookup(&self, id: &str) -> Option<PathBuf> {
        let mut cache = self.0.lock().expect("icon cache lock");

        if !cache.contains_key(id) {
            if let Some(path) = lookup(id) {
                cache.insert(id.to_string(), path);
            }
        }

        cache.get(id).cloned()
    }
}

fn lookup(id: &str) -> Option<PathBuf> {
    if let Some(icon) = lookup_icon(id) {
        return Some(icon);
    }

    // KDE applications are special, so we'll go hunt for them ourselves. Again, this is loosely
    // adapted from wlr/taskbar.
    for dir in XDG_DATA_DIRS.iter() {
        for prefix in [
            "applications/",
            "applications/kde/",
            "applications/org.kde.",
        ] {
            for suffix in ["", ".desktop"] {
                let path = dir.join(format!("{prefix}{id}{suffix}"));
                if let Some(info) = DesktopAppInfo::from_filename(&path) {
                    if let Some(path) = info.icon_path() {
                        return Some(path);
                    }
                }
            }
        }
    }

    // This is _very_ roughly adapted from the wlr/taskbar module built into Waybar. We don't do
    // the same startup_wm_class check here for now.
    let infos = DesktopAppInfo::search(id);
    for possible in infos.into_iter().flatten() {
        if let Some(info) = DesktopAppInfo::new(&possible) {
            if let Some(path) = info.icon_path() {
                return Some(path);
            }
        }
    }

    None
}

fn lookup_icon(id: &str) -> Option<PathBuf> {
    if let Some(path) = freedesktop_icons::lookup(id).with_size(512).find() {
        return Some(path);
    }

    if let Some(path) = linicon::lookup_icon(id)
        .with_size(512)
        .filter_map(|result| result.ok())
        .next()
    {
        return Some(path.path);
    }

    None
}

static XDG_DATA_DIRS: LazyLock<Vec<PathBuf>> = LazyLock::new(|| {
    let mut dirs = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }

    if let Ok(env) = std::env::var("XDG_DATA_DIRS") {
        dirs.extend(env.split(':').map(PathBuf::from))
    } else {
        dirs.extend(
            ["/usr/share", "/usr/local/share"]
                .into_iter()
                .map(PathBuf::from),
        );
    }

    dirs
});

trait DesktopAppInfoExt {
    fn icon_path(&self) -> Option<PathBuf>;
}

impl DesktopAppInfoExt for DesktopAppInfo {
    fn icon_path(&self) -> Option<PathBuf> {
        self.icon()
            .and_then(|icon| IconExt::to_string(&icon))
            .and_then(|name| lookup_icon(&name))
    }
}
