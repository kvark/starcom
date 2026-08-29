//! Headless building blocks for the Starcom tmux GUI.
//!
//! This foundation deliberately owns neither an SSH connection nor a window.
//! See PLAN.md before adding a transport, renderer, or recovery behavior.

pub mod command;
pub mod connection;
pub mod control;
pub mod core;
pub mod replay;
pub mod terminal;
