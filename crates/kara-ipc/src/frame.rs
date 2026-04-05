//! Length-prefixed JSON framing for IPC messages.
//!
//! Wire format: 4-byte u32 LE length + JSON payload bytes.

use std::io::{Read, Write};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Maximum message size (64 KB). Prevents unbounded allocations.
const MAX_MESSAGE_SIZE: u32 = 64 * 1024;

/// Read a length-prefixed JSON message from a stream.
pub fn read_message<T: for<'de> Deserialize<'de>, R: Read>(reader: &mut R) -> Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        bail!("message too large: {len} bytes (max {MAX_MESSAGE_SIZE})");
    }

    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;

    let msg = serde_json::from_slice(&payload)?;
    Ok(msg)
}

/// Write a length-prefixed JSON message to a stream.
pub fn write_message<T: Serialize, W: Write>(writer: &mut W, msg: &T) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    let len = payload.len() as u32;

    if len > MAX_MESSAGE_SIZE {
        bail!("message too large: {len} bytes (max {MAX_MESSAGE_SIZE})");
    }

    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}
