//! Building blocks for the Starcom tmux GUI.
//!
//! Protocol and terminal modules own neither a network connection nor a window.
//! Optional SSH and desktop modules add live, read-only views.

pub mod command;
pub mod connection;
pub mod control;
pub mod core;
#[cfg(feature = "ssh")]
pub mod inspect;
pub mod replay;
#[cfg(feature = "ssh")]
pub mod session;
pub mod snapshot;
#[cfg(feature = "ssh")]
pub mod ssh;
pub mod terminal;

#[cfg(feature = "gui")]
pub mod desktop;
#[cfg(feature = "gui")]
mod ui;
#[cfg(feature = "gui")]
mod window;
