//! Read-only local service stream. The collector remains the only BLE/storage owner.
use crate::{config::Config, metrics::Workout};
use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, watch, Semaphore},
    task::{JoinHandle, JoinSet},
    time::timeout,
};

pub const DEFAULT_SOCKET: &str = "/run/sunnyfit-ble-cli/stream.sock";
const MAX_LINE: usize = 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    pub source_run_id: String,
    pub source_sequence: u64,
    pub device: Option<String>,
    pub adapter: Option<String>,
    pub demo: bool,
    pub workout: Workout,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
    Snapshot { snapshot: Box<Snapshot> },
    Event { event: Value },
    Gap { reason: String },
}
struct State {
    snapshot: Snapshot,
    tx: broadcast::Sender<Arc<String>>,
}
pub struct Hub {
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
    path: PathBuf,
    inode: u64,
    _lock: File,
}
fn line(message: &Message) -> Result<String> {
    Ok(serde_json::to_string(message)? + "\n")
}
async fn send(stream: &mut UnixStream, text: &str) -> Result<()> {
    timeout(Duration::from_secs(2), stream.write_all(text.as_bytes()))
        .await
        .context("live subscriber stalled")??;
    Ok(())
}
impl Hub {
    pub fn start(c: &Config, run_id: &str, demo: bool) -> Result<Self> {
        let path = c.stream_socket.clone();
        let parent = path
            .parent()
            .context("stream socket needs a parent directory")?;
        crate::config::private_dir(parent)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .context("another collector already owns this live stream")?;
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                if !m.file_type().is_socket() {
                    bail!("stream socket path is occupied by a non-socket file");
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path).context("cannot bind local live stream")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
        let inode = fs::symlink_metadata(&path)?.ino();
        let (tx, _) = broadcast::channel(256);
        let state = Arc::new(Mutex::new(State {
            snapshot: Snapshot {
                protocol: 1,
                source_run_id: run_id.into(),
                source_sequence: 0,
                device: c.device.clone(),
                adapter: c.adapter.clone(),
                demo,
                workout: Workout::default(),
            },
            tx,
        }));
        let shared = state.clone();
        let slots = Arc::new(Semaphore::new(32));
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    result=listener.accept()=>{
                        let Ok((mut stream,_))=result else{break;};
                        let Ok(permit)=slots.clone().try_acquire_owned()else{continue;};
                        let shared=shared.clone();
                        clients.spawn(async move {
                            let _permit=permit;
                            let(mut rx,first)={let s=shared.lock().unwrap();(s.tx.subscribe(),line(&Message::Snapshot{snapshot:Box::new(s.snapshot.clone())})?)};
                            send(&mut stream,&first).await?;
                            loop{match rx.recv().await{
                                Ok(text)=>send(&mut stream,&text).await?,
                                Err(broadcast::error::RecvError::Lagged(_))=>{
                                    let first={let s=shared.lock().unwrap();rx=s.tx.subscribe();line(&Message::Snapshot{snapshot:Box::new(s.snapshot.clone())})?};
                                    send(&mut stream,&line(&Message::Gap{reason:"live stream subscriber fell behind; counters rebased".into()})?).await?;
                                    send(&mut stream,&first).await?;
                                },Err(broadcast::error::RecvError::Closed)=>return Ok::<(),anyhow::Error>(()),
                            }}
                        });
                    },_ = clients.join_next(),if !clients.is_empty()=>{}
                }
            }
        });
        Ok(Self {
            state,
            task,
            path,
            inode,
            _lock: lock,
        })
    }
    pub fn publish(&self, row: &Value, workout: &Workout) {
        let mut state = self.state.lock().unwrap();
        let message = if matches!(
            row["type"].as_str(),
            Some("metrics" | "connection" | "ready" | "workout_summary")
        ) {
            state.snapshot.workout = workout.clone();
            state.snapshot.source_sequence = row["sequence"].as_u64().unwrap_or(0);
            Message::Snapshot {
                snapshot: Box::new(state.snapshot.clone()),
            }
        } else {
            Message::Event { event: row.clone() }
        };
        if let Ok(text) = line(&message) {
            let _ = state.tx.send(Arc::new(text));
        }
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.ino() == self.inode && m.file_type().is_socket())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct Attachment {
    reader: BufReader<UnixStream>,
    pub first: Snapshot,
}
async fn receive(reader: &mut BufReader<UnixStream>) -> Result<Message> {
    let mut bytes = Vec::new();
    let mut limited = reader.take(MAX_LINE as u64 + 1);
    let n = timeout(
        Duration::from_secs(5),
        limited.read_until(b'\n', &mut bytes),
    )
    .await
    .context("service stream heartbeat timed out")??;
    if n == 0 {
        bail!("service stream closed");
    }
    if n > MAX_LINE || bytes.last() != Some(&b'\n') {
        bail!("invalid service stream record size");
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid service stream message"))
}
async fn greeting(stream: UnixStream) -> Result<Attachment> {
    let mut reader = BufReader::new(stream);
    let Message::Snapshot { snapshot } = receive(&mut reader).await? else {
        bail!("service stream did not send a snapshot");
    };
    if snapshot.protocol != 1 {
        bail!("unsupported live stream protocol; update client and service together");
    }
    Ok(Attachment {
        reader,
        first: *snapshot,
    })
}
fn matches_device(wanted: Option<&str>, actual: Option<&str>) -> bool {
    wanted.is_none_or(|w| actual.is_some_and(|a| w.eq_ignore_ascii_case(a)))
}
fn validate(c: &Config, s: &Snapshot) -> Result<()> {
    if !matches_device(c.device.as_deref(), s.device.as_deref())
        || !matches_device(c.adapter.as_deref(), s.adapter.as_deref())
    {
        bail!("the service is collecting a different bike or adapter; select its device, or use --workout-source direct for a different bike");
    }
    Ok(())
}
async fn service_active() -> bool {
    let check = tokio::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "sunnyfit-ble-cli.service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(timeout(Duration::from_secs(2),check).await,Ok(Ok(status))if status.success())
}
/// Called before opening a TUI or touching Bluetooth. Absence permits standalone use.
pub async fn probe(c: &Config) -> Result<Option<Attachment>> {
    match timeout(
        Duration::from_secs(2),
        UnixStream::connect(&c.stream_socket),
    )
    .await
    {
        Ok(Ok(stream)) => {
            let a = greeting(stream).await?;
            if c.workout_source == "direct" {
                let different_bike = c
                    .device
                    .as_deref()
                    .zip(a.first.device.as_deref())
                    .is_some_and(|(wanted, actual)| !wanted.eq_ignore_ascii_case(actual));
                if different_bike {
                    return Ok(None);
                }
                bail!("the service already owns this bike; use --workout-source auto to share its stream");
            }
            validate(c, &a.first)?;
            Ok(Some(a))
        }
        Ok(Err(e)) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            if c.workout_source == "service" {
                bail!("service stream unavailable; start the updated service or use --workout-source auto for standalone Bluetooth");
            }
            if c.stream_socket == Path::new(DEFAULT_SOCKET) && service_active().await {
                bail!("the service is active but its live stream is unavailable; restart the updated service before opening a workout");
            }
            Ok(None)
        }
        Ok(Err(e)) if e.kind() == ErrorKind::PermissionDenied => {
            bail!("permission denied opening the service stream; join the sunnyfit-ble-cli group and log in again")
        }
        _ => bail!("cannot open local service stream"),
    }
}
async fn forward(tx: &mpsc::Sender<Value>, mut row: Value, epoch: u64) -> Result<()> {
    row["epoch"] = json!(epoch);
    tx.send(row)
        .await
        .map_err(|_| anyhow!("workout reader stopped"))
}
async fn snapshot(tx: &mpsc::Sender<Value>, s: Snapshot, epoch: u64) -> Result<()> {
    forward(tx, json!({"type":"service_snapshot","snapshot":s}), epoch).await
}
pub async fn follow(
    c: Config,
    mut attached: Attachment,
    tx: mpsc::Sender<Value>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let identity = attached.first.clone();
    let mut epoch = 1;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let session = async {
            snapshot(&tx, attached.first.clone(), epoch).await?;
            loop {
                match receive(&mut attached.reader).await? {
                    Message::Snapshot { snapshot: s } => {
                        validate(&c, &s)?;
                        if s.demo != identity.demo
                            || s.device != identity.device
                            || s.adapter != identity.adapter
                        {
                            bail!("service source identity changed");
                        }
                        snapshot(&tx, *s, epoch).await?;
                    }
                    Message::Event { event } => {
                        forward(&tx, json!({"type":"service_event","event":event}), epoch).await?
                    }
                    Message::Gap { reason } => {
                        forward(&tx, json!({"type":"coverage_gap","reason":reason}), epoch).await?;
                        forward(
                            &tx,
                            json!({"type":"connection","state":"stream gap"}),
                            epoch,
                        )
                        .await?;
                    }
                }
            }
        };
        let _result: Result<()> =
            tokio::select! {_=stop.changed()=>return Ok(()),result=session=>result};
        forward(&tx,json!({"type":"coverage_gap","reason":"service stream interrupted; waiting to reattach"}),epoch).await?;
        forward(
            &tx,
            json!({"type":"connection","state":"waiting for service"}),
            epoch,
        )
        .await?;
        loop {
            let retry = async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let stream = UnixStream::connect(&c.stream_socket).await?;
                let a = greeting(stream).await?;
                validate(&c, &a.first)?;
                if a.first.demo != identity.demo
                    || a.first.device != identity.device
                    || a.first.adapter != identity.adapter
                {
                    bail!("service source identity changed");
                }
                Ok::<_, anyhow::Error>(a)
            };
            tokio::select! {_=stop.changed()=>return Ok(()),result=retry=>{if let Ok(a)=result{attached=a;epoch+=1;break;}}}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn snapshot_is_immediate_and_socket_has_private_group_permissions() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config {
            stream_socket: dir.join("stream.sock"),
            ..Default::default()
        };
        let hub = Hub::start(&c, "test", true).unwrap();
        assert_eq!(
            fs::metadata(&c.stream_socket).unwrap().permissions().mode() & 0o777,
            0o660
        );
        let mut w = Workout::default();
        w.connection("ready");
        hub.publish(&json!({"type":"metrics","sequence":1}), &w);
        let a = probe(&c).await.unwrap().unwrap();
        assert_eq!(a.first.workout.connection, "ready");
        assert!(a.first.demo);
        assert!(Hub::start(&c, "second", false).is_err());
        drop(a);
        drop(hub);
        assert!(!c.stream_socket.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn source_selection_rejects_another_bike() {
        assert!(matches_device(None, Some("x")));
        assert!(matches_device(Some("a"), Some("A")));
        assert!(!matches_device(Some("a"), Some("b")));
    }
}
