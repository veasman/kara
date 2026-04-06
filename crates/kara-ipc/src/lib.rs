//! kara-ipc: IPC protocol for the kara desktop environment.
//!
//! Provides message types, framing helpers, client connector, and server listener
//! for Unix socket communication between kara-gate and other tools.
//!
//! Socket: `$XDG_RUNTIME_DIR/kara.sock`
//! Format: 4-byte u32 LE length prefix + JSON payload

pub mod message;
pub mod frame;
pub mod client;
pub mod server;

pub use message::*;
pub use frame::{read_message, write_message};
pub use client::IpcClient;

use std::env;
use std::path::PathBuf;

/// Return the IPC socket path: `$XDG_RUNTIME_DIR/kara.sock`
pub fn socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("kara.sock")
}
