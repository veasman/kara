//! IPC client for tools connecting to kara-gate.
//!
//! The server (kara-gate) reads exactly one request per connection and
//! closes the socket after writing the response — that's the simplest
//! thing to do from inside the non-blocking per-frame accept loop.
//! So every `request()` and `send()` call opens a fresh UnixStream.
//! Callers that fire several requests in a row (kara-glimpse multi-
//! output region capture, kara-veil theme+outputs probe) would
//! otherwise hit `Broken pipe` on the second write.

use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};

use crate::frame::{read_message, write_message};
use crate::message::{Request, Response};

/// Blocking IPC client that connects to the kara-gate compositor.
///
/// The struct carries no socket state — it exists so callers can pass
/// a `&mut IpcClient` through helpers and for symmetry with future
/// persistent-subscription APIs. Each request is its own connection.
pub struct IpcClient {
    _priv: (),
}

impl IpcClient {
    /// Probe the compositor socket. Returns Err if the socket file
    /// doesn't exist or the compositor isn't accepting. Doesn't keep
    /// the connection open — per-request connections (see module docs)
    /// handle the actual traffic.
    pub fn connect() -> Result<Self> {
        let path = crate::socket_path();
        // Probe: connect + immediately drop. Confirms the socket is
        // live and the caller can proceed to request().
        let _ = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        Ok(Self { _priv: () })
    }

    /// Send a request and wait for a response. Opens a fresh connection
    /// per call — the server reads one request per connection.
    pub fn request(&mut self, req: &Request) -> Result<Response> {
        let path = crate::socket_path();
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        write_message(&mut stream, req)?;
        read_message(&mut stream)
    }

    /// Send a request without waiting for a response (fire-and-forget).
    /// Opens a fresh connection per call; see `request()` for why.
    pub fn send(&mut self, req: &Request) -> Result<()> {
        let path = crate::socket_path();
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        write_message(&mut stream, req)
    }
}
