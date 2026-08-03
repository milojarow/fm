//! Small, general purpose file manager built using GTK.
//!
//! Generally, each top-level module corresponds to a different Relm4 component.

#![warn(clippy::dbg_macro)]
#![warn(clippy::print_stderr)]
#![warn(clippy::print_stdout)]
#![warn(clippy::todo)]

pub mod audio;
pub mod clipboard;
mod component;
mod config;
pub mod layout;
pub mod ops;
pub mod path_title;
pub mod transfer;
mod util;

pub use component::app::AppModel;
