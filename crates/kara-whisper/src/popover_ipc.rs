//! Popover socket listener for kara-whisper.
//!
//! Beautify (and anything else in the kara family that wants to
//! surface an ephemeral "just happened" message) writes to
//! `$XDG_RUNTIME_DIR/kara-whisper-popover.sock`. Each request is a
//! length-prefixed JSON payload matching `PopoverRequest`.
//!
//! We accept connections on a dedicated thread and forward each
//! request through an mpsc channel into the main whisper event loop,
//! which renders it alongside normal D-Bus notifications. Keeping the
//! socket listener off the Wayland thread means the main loop never
//! blocks on accept() and can still service notifications while
//! waiting for popover traffic.

use std::env;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use serde::{Deserialize, Serialize};

/// Protocol — must stay in sync with kara-beautify's popover.rs
/// client. Small enough that duplicating across two crates is
/// cheaper than a shared crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PopoverRequest {
    Show { text: String, duration_ms: u32 },
    Hide,
}

/// Main-loop event surfaced from the popover listener thread.
#[derive(Debug, Clone)]
pub enum PopoverEvent {
    Show { text: String, duration_ms: u32 },
    Hide,
}

pub fn socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("kara-whisper-popover.sock")
}

/// Spawn the listener thread. Returns immediately — the thread runs
/// for the lifetime of the whisper process, accepting connections in
/// a blocking loop and forwarding PopoverEvents into the channel.
/// Returns a JoinHandle the main loop can ignore (dropping it lets
/// the thread detach).
pub fn spawn(tx: Sender<PopoverEvent>) -> Option<thread::JoinHandle<()>> {
    let path = socket_path();

    // Remove any stale socket from a crashed previous whisper.
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "kara-whisper: failed to bind popover socket {}: {e}",
                path.display()
            );
            return None;
        }
    };

    Some(thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            let req: PopoverRequest = match kara_ipc::read_message(&mut stream) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let event = match req {
                PopoverRequest::Show { text, duration_ms } => {
                    PopoverEvent::Show { text, duration_ms }
                }
                PopoverRequest::Hide => PopoverEvent::Hide,
            };
            if tx.send(event).is_err() {
                // Main thread dropped the receiver — whisper is exiting.
                break;
            }
        }
        let _ = std::fs::remove_file(&path);
    }))
}
