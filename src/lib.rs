//! lilypad: a keyboard-driven floating window manager for macOS. Windows stay
//! where they are put, and go where they are sent with a smooth animation.

pub mod command;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod geometry;
pub mod hotkeys;
pub mod ipc;
pub mod manager;
pub mod motion;
pub mod place;
pub mod platform;
pub mod service;
