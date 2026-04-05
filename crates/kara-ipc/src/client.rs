//! IPC client for tools connecting to kara-gate.

use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};

use crate::frame::{read_message, write_message};
use crate::message::{Request, Response};

/// Blocking IPC client that connects to the kara-gate compositor.
pub struct IpcClient {
    stream: UnixStream,
}

impl IpcClient {
    /// Connect to the compositor's IPC socket.
    pub fn connect() -> Result<Self> {
        let path = crate::socket_path();
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("failed to connect to {}", path.display()))?;
        Ok(Self { stream })
    }

    /// Send a request and wait for a response.
    pub fn request(&mut self, req: &Request) -> Result<Response> {
        write_message(&mut self.stream, req)?;
        read_message(&mut self.stream)
    }

    /// Send a request without waiting for a response (fire-and-forget).
    pub fn send(&mut self, req: &Request) -> Result<()> {
        write_message(&mut self.stream, req)
    }
}
