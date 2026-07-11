use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::*;

/// Whether hidden files are currently shown. Global because every directory
/// panel's filter and the window action need to agree on it.
static SHOW_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Returns whether hidden files are currently shown.
pub fn show_hidden() -> bool {
    SHOW_HIDDEN.load(Ordering::Relaxed)
}

/// Sets whether hidden files are shown.
pub fn set_show_hidden(show: bool) {
    SHOW_HIDDEN.store(show, Ordering::Relaxed);
}

/// Application state that is not intended to be directly configurable by the user. The state is
/// converted to and from JSON, and stored in the platform's application directory. It is not
/// updated during application execution.
///
/// We could use [`gio::Settings`] for this, but for now this is simpler than installing and
/// managing schemas.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Width of the main window at startup.
    pub width: i32,

    /// Height of the main window at startup.
    pub height: i32,

    /// Whether the window should be maximized at startup.
    pub is_maximized: bool,

    /// Whether hidden files should be shown at startup.
    pub show_hidden: bool,
}

impl State {
    /// Read from the state file on disk.
    pub fn read() -> Result<Self> {
        let path = state_path()?;
        Ok(serde_json::from_reader(File::open(path)?)?)
    }

    /// Persist to disk.
    pub fn write(&self) -> Result<()> {
        info!("persisting application state: {:?}", self);

        let path = state_path()?;

        fs::create_dir_all(path.parent().unwrap())?;

        let file = File::create(path)?;
        Ok(serde_json::to_writer(file, self)?)
    }
}

impl Default for State {
    fn default() -> Self {
        State {
            width: 900,
            height: 600,
            is_maximized: false,
            show_hidden: false,
        }
    }
}

fn state_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("io", "eucl", "fm")
        .ok_or_else(|| anyhow!("unable to find user home directory"))?;
    Ok(dirs.data_local_dir().join("state.json"))
}
