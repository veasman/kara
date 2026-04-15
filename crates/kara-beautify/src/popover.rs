//! kara-whisper popover client — fires ephemeral on-screen hints.
//!
//! The beautify daemon fires a popover after a successful variant
//! cycle so the user gets instant feedback ("Default: Nord") without
//! having to open the picker. The socket lives at
//! `$XDG_RUNTIME_DIR/kara-whisper-popover.sock` and accepts the
//! lightweight protocol defined in kara-whisper's `popover_ipc`
//! module.
//!
//! This is a best-effort client — failures are logged and swallowed.
//! kara-whisper may not be running yet on a fresh session, or the
//! user may have the popover consumer disabled. Neither case should
//! make `kara-beautify toggle` fail.

use std::env;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

fn socket_path() -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("kara-whisper-popover.sock")
}

/// Protocol enum — kept in sync with kara-whisper's side. Small
/// enough that duplication is cheaper than a shared crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PopoverRequest {
    Show {
        text: String,
        duration_ms: u32,
    },
    Hide,
}

/// Best-effort: try to connect to kara-whisper's popover socket and
/// fire a Show. Silently fails if the socket doesn't exist or the
/// write stalls.
pub fn try_show(text: &str, duration_ms: u32) {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));

    let req = PopoverRequest::Show {
        text: text.to_string(),
        duration_ms,
    };
    let _ = kara_ipc::write_message(&mut stream, &req);
}
