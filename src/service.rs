use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::OnceLock,
};
static LOG: OnceLock<PathBuf> = OnceLock::new();
pub fn init(path: &Path) -> Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(path)
        .context("cannot open service log")?;
    let _ = LOG.set(path.to_owned());
    log("starting", json!({"version":env!("CARGO_PKG_VERSION")}));
    Ok(())
}
/// Callers supply fixed messages and counters, never connection strings or server error bodies.
pub fn log(event: &str, details: Value) {
    if let Some(path) = LOG.get() {
        let row =
            json!({"timestamp_unix_ms":crate::storage::unix_ms(),"event":event,"details":details});
        // Reopen for every entry so logrotate needs no signal or copytruncate.
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o640)
            .open(path)
        {
            let _ = writeln!(f, "{row}");
        }
    }
}
