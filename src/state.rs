use std::sync::Arc;

use crate::{config::Config, icon, niri::Niri};

/// Global state for the taskbar.
#[derive(Debug, Clone)]
pub struct State(Arc<Inner>);

impl State {
    /// Instantiates the global state.
    pub fn new(config: Config) -> Self {
        Self(Arc::new(Inner {
            config: config.clone(),
            icon_cache: icon::Cache::default(),
            niri: Niri::new(config),
        }))
    }

    /// Returns the taskbar configuration.
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    /// Accesses the global icon cache.
    pub fn icon_cache(&self) -> &icon::Cache {
        &self.0.icon_cache
    }

    /// Accesses the global [`Niri`] instance.
    pub fn niri(&self) -> &Niri {
        &self.0.niri
    }
}

#[derive(Debug)]
struct Inner {
    config: Config,
    icon_cache: icon::Cache,
    niri: Niri,
}
