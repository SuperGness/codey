//! Shared stdin/stdout plumbing for the Codex Hook entry points
//! (`subagent_gate` and `fastctx_route_gate`). Both run as short-lived
//! processes that read one JSON payload and answer with one JSON line.

use std::io::{Read, Write};

use anyhow::{Context, Result};
use serde_json::Value;

/// Reads at most `max_bytes + 1` bytes so callers can detect an oversized
/// payload by comparing the length against `max_bytes`.
pub(crate) fn read_stdin_bounded(max_bytes: u64, context: &'static str) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(max_bytes + 1)
        .read_to_end(&mut raw)
        .context(context)?;
    Ok(raw)
}

pub(crate) fn write_output(output: &Value, context: &'static str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, output).context(context)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
