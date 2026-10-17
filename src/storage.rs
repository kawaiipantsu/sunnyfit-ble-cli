use crate::config::{private_dir, write_private};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
pub fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err(anyhow!("invalid hex payload"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}
pub struct Recorder {
    pub dir: PathBuf,
    pub run_id: String,
    pub sequence: u64,
    tx: SyncSender<String>,
    worker: thread::JoinHandle<Result<()>>,
}
impl Recorder {
    pub fn new(base: &Path, demo: bool) -> Result<Self> {
        private_dir(base)?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let dir = base.join(format!("{}-{}", unix_ms(), run_id));
        private_dir(&dir)?;
        write_private(
            &dir.join("manifest.json"),
            &serde_json::to_vec_pretty(
                &json!({"schema":"sunnyfit.manifest.v1","run_id":run_id,"version":env!("CARGO_PKG_VERSION"),"model":"SF-B023005DGY","demo":demo,"status":"in_progress","scope":"BlueZ discovery, GATT inventory/reads and available notify/indicate sources","limits":["BlueZ property snapshots are not raw advertisements","fallback StartNotify can miss initial update; use btmon","disconnect gaps and upstream losses cannot be counted"],"durability":"bounded queue; flush and sync_data every second; final sync; crash tail may be lost"}),
            )?,
        )?;
        let f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("events.jsonl"))?;
        let (tx, rx) = mpsc::sync_channel::<String>(4096);
        let worker = thread::spawn(move || {
            let mut f = BufWriter::new(f);
            let mut sync = Instant::now();
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(line) => writeln!(f, "{line}")?,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if sync.elapsed() >= Duration::from_secs(1) {
                    f.flush()?;
                    f.get_ref().sync_data()?;
                    sync = Instant::now();
                }
            }
            f.flush()?;
            f.get_ref().sync_all()?;
            Ok(())
        });
        Ok(Self {
            dir,
            run_id,
            sequence: 0,
            tx,
            worker,
        })
    }
    pub fn record(&mut self, row: &Value) -> Result<()> {
        self.sequence += 1;
        self.tx.try_send(serde_json::to_string(row)?).map_err(|_| {
            anyhow!("local recorder queue full or writer failed; capture is partial")
        })?;
        Ok(())
    }
    pub fn finish(self, mut summary: Value) -> Result<()> {
        drop(self.tx);
        let result = self
            .worker
            .join()
            .map_err(|_| anyhow!("recorder thread failed"))?;
        if result.is_err() {
            summary["capture_status"] = json!("partial");
            summary["storage_error"] = json!(true);
        }
        write_private(
            &self.dir.join("summary.json"),
            &serde_json::to_vec_pretty(&summary)?,
        )?;
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(self.dir.join("manifest.json"))?)?;
        manifest["status"] = summary["capture_status"].clone();
        manifest["events"] = json!(self.sequence);
        write_private(
            &self.dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        result.context("writing local capture")
    }
}
