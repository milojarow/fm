use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::*;

/// Whether hidden files are currently shown. Global because every directory
/// panel's filter and the window action need to agree on it.
static SHOW_HIDDEN: AtomicBool = AtomicBool::new(false);

/// The active sort key, stored as its discriminant.
static SORT_KEY: AtomicU8 = AtomicU8::new(SortKey::Name as u8);

/// Whether the current sort order is reversed.
static SORT_REVERSED: AtomicBool = AtomicBool::new(false);

/// What directory entries are ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum SortKey {
    /// Case-insensitive name.
    #[default]
    Name = 0,
    /// Modification time.
    Modified = 1,
    /// Directories first, then grouped by content type; name breaks ties.
    Type = 2,
}

impl SortKey {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => SortKey::Modified,
            2 => SortKey::Type,
            _ => SortKey::Name,
        }
    }
}

/// Returns whether hidden files are currently shown.
pub fn show_hidden() -> bool {
    SHOW_HIDDEN.load(Ordering::Relaxed)
}

/// Sets whether hidden files are shown.
pub fn set_show_hidden(show: bool) {
    SHOW_HIDDEN.store(show, Ordering::Relaxed);
}

/// Returns the active sort key.
pub fn sort_key() -> SortKey {
    SortKey::from_u8(SORT_KEY.load(Ordering::Relaxed))
}

/// Sets the active sort key.
pub fn set_sort_key(key: SortKey) {
    SORT_KEY.store(key as u8, Ordering::Relaxed);
}

/// Returns whether the sort order is reversed.
pub fn sort_reversed() -> bool {
    SORT_REVERSED.load(Ordering::Relaxed)
}

/// Sets whether the sort order is reversed.
pub fn set_sort_reversed(reversed: bool) {
    SORT_REVERSED.store(reversed, Ordering::Relaxed);
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

    /// The sort key entries should be ordered by at startup.
    pub sort_key: SortKey,

    /// Whether the sort order should be reversed at startup.
    pub sort_reversed: bool,
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
            sort_key: SortKey::default(),
            sort_reversed: false,
        }
    }
}

fn state_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("io", "eucl", "fm")
        .ok_or_else(|| anyhow!("unable to find user home directory"))?;
    Ok(dirs.data_local_dir().join("state.json"))
}
