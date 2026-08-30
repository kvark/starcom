//! Building blocks for the Starcom tmux GUI.
//!
//! Protocol and terminal modules own neither a network connection nor a window.
//! Optional SSH and desktop modules add explicitly authorized live sessions.

pub mod command;
pub mod connection;
pub mod control;
pub mod core;
pub mod input;
#[cfg(feature = "ssh")]
pub mod inspect;
pub mod replay;
#[cfg(feature = "ssh")]
pub mod session;
pub mod snapshot;
#[cfg(feature = "ssh")]
pub mod ssh;
#[cfg(feature = "ssh")]
pub mod ssh_config;
pub mod terminal;

#[cfg(feature = "gui")]
pub mod desktop;
#[cfg(feature = "gui")]
mod ui;
#[cfg(feature = "gui")]
mod window;
#[cfg(feature = "gui")]
mod window_runtime;
#[cfg(feature = "gui")]
mod workspace;
