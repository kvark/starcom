//! Building blocks for the Starcom tmux GUI.
//!
//! Protocol and terminal modules own neither a network connection nor a window.
//! The optional SSH backend and read-only inspector are the first live slice.

pub mod command;
pub mod connection;
pub mod control;
pub mod core;
#[cfg(feature = "ssh")]
pub mod inspect;
pub mod replay;
#[cfg(feature = "ssh")]
pub mod ssh;
pub mod terminal;
