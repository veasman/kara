//! IPC server helpers for kara-gate.
//!
//! Provides socket creation and per-connection message handling.
//! The calloop integration lives in kara-gate (ipc.rs) since it
//! depends on smithay's event loop.

use std::fs;
use std::os::unix::net::UnixListener;

use anyhow::{Context, Result};

/// Create and bind the IPC socket, removing any stale socket file.
pub fn bind_socket() -> Result<UnixListener> {
    let path = crate::socket_path();

    // Remove stale socket if it exists
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove stale socket: {}", path.display()))?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind IPC socket: {}", path.display()))?;

    listener.set_nonblocking(true)?;

    tracing::info!("IPC listening on {}", path.display());
    Ok(listener)
}

/// Clean up the socket file on shutdown.
pub fn cleanup_socket() {
    let path = crate::socket_path();
    let _ = fs::remove_file(&path);
}
