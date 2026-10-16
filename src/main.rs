mod ble;
mod config;
mod metrics;
mod publish;
mod storage;
mod sunny;
mod tui;
use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use config::{Cli, Command, Config};
use metrics::{Decoder, Workout};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc as sync_mpsc, Arc,
    },
    time::{Duration, Instant},
};
use storage::{unhex, Recorder};
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};

struct Output {
    tx: sync_mpsc::SyncSender<String>,
    done: sync_mpsc::Receiver<()>,
    lost: Arc<AtomicU64>,
}
impl Output {
    fn new() -> Self {
        let (tx, rx) = sync_mpsc::sync_channel::<String>(1024);
        let (done_tx, done) = sync_mpsc::channel();
        let lost = Arc::new(AtomicU64::new(0));
        let l = lost.clone();
        std::thread::spawn(move || {
            let mut out = std::io::stdout().lock();
            for s in rx {
                if writeln!(out, "{s}").and_then(|_| out.flush()).is_err() {
                    l.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
            let _ = done_tx.send(());
        });
        Self { tx, done, lost }
    }
    fn send(&self, v: &Value) {
        if self.tx.try_send(v.to_string()).is_err() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn finish(self) -> u64 {
        drop(self.tx);
        if self.done.recv_timeout(Duration::from_secs(2)).is_err() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
        self.lost.load(Ordering::Relaxed)
    }
}
struct Engine {
    rec: Recorder,
    publisher: Option<publish::Publisher>,
    output: Option<Output>,
    c: Config,
    workout: Workout,
    decoders: HashMap<String, Decoder>,
    epoch: u64,
    raw: u64,
    unknown: u64,
    coverage_errors: u64,
    gaps: u64,
}
impl Engine {
    fn emit(&mut self, row: Value, ms: u64, channel: &str) -> Result<Value> {
        let row = self.rec.record(row, ms)?;
        if let Some(p) = &mut self.publisher {
            p.enqueue(channel, &row);
        }
        if self.c.debug_stdout {
            if let Some(out) = &self.output {
                if self.c.verbose
                    || matches!(
                        row["type"].as_str(),
                        Some("connection" | "ready" | "retry" | "workout_summary")
                    )
                {
                    out.send(&row);
                }
            }
        }
        Ok(row)
    }
    fn input(&mut self, row: Value, ms: u64) -> Result<()> {
        let epoch = row["epoch"].as_u64().unwrap_or(0);
        if epoch != self.epoch {
            self.decoders.clear();
            self.epoch = epoch;
        }
        let kind = row["type"].as_str().unwrap_or("").to_owned();
        if kind == "connection" {
            self.workout
                .connection(row["state"].as_str().unwrap_or("unknown"));
        }
        if kind == "coverage_gap" {
            self.gaps += 1;
        }
        if kind.ends_with("error") || row["outcome"] == "failed" {
            self.coverage_errors += 1;
        }
        let is_value = matches!(
            kind.as_str(),
            "notification" | "gatt_value_update" | "read_result"
        );
        let channel = if matches!(
            kind.as_str(),
            "connection" | "coverage_gap" | "retry" | "ready"
        ) {
            self.c.status_channel.clone()
        } else {
            self.c.raw_channel.clone()
        };
        let row = self.emit(row, ms, &channel)?;
        if is_value {
            self.raw += 1;
            let uuid = row["characteristic_uuid"].as_str().unwrap_or("");
            let source = format!(
                "{}:{}:{}",
                row["service_id"], row["characteristic_id"], uuid
            );
            let bytes = unhex(row["value_hex"].as_str().unwrap_or(""))?;
            let observed = row["observed_elapsed_ms"].as_u64().unwrap_or(ms);
            let decoder = self.decoders.entry(source).or_default();
            let result = decoder.decode_event(
                uuid,
                &bytes,
                observed,
                &self.c,
                row["sequence"].as_u64().unwrap_or(0),
            );
            let pending = decoder.pending();
            match result {
                Ok(Some(sample)) => {
                    let decoded =
                        json!({"type":"decoded","source_event":row["sequence"],"sample":sample});
                    self.workout
                        .ingest(sample, ms, row["sequence"].as_u64().unwrap(), &self.c);
                    let ch = self.c.metrics_channel.clone();
                    self.emit(decoded, ms, &ch)?;
                }
                Ok(None) => {
                    if !pending {
                        self.unknown += 1;
                    }
                }
                Err(e) => {
                    self.coverage_errors += 1;
                    let ch = self.c.status_channel.clone();
                    self.emit(json!({"type":"decode_error","source_event":row["sequence"],"error":e.to_string()}),ms,&ch)?;
                }
            }
        }
        Ok(())
    }
}
async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
}
fn address(c: &Config, explicit: Option<bluer::Address>) -> Result<bluer::Address> {
    explicit
        .or_else(|| c.device.as_deref().and_then(|s| s.parse().ok()))
        .context("choose a bike with --device ADDRESS, or run scan then connect ADDRESS")
}
fn base_dir(c: &Config, config: &Path) -> PathBuf {
    c.output_dir.clone().unwrap_or_else(|| {
        config
            .parent()
            .unwrap_or(Path::new("."))
            .join("past-workouts")
    })
}
async fn demo(out: ble::Events, mut stop: watch::Receiver<bool>) -> Result<()> {
    out.state("ready").await?;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut n = 0u16;
    loop {
        tokio::select! {_=stop.changed()=>break,_=interval.tick()=>{
            let rpm=55.+(n as f64/5.).sin()*12.;let speed=(rpm*0.3*100.) as u16;
            let mut bytes=vec![0x44,0];bytes.extend(speed.to_le_bytes());bytes.extend(((rpm*2.) as u16).to_le_bytes());bytes.extend(65u16.to_le_bytes());
            out.send(json!({"type":"notification","demo":true,"service_id":1,"characteristic_id":2,"characteristic_uuid":"00002ad2-0000-1000-8000-00805f9b34fb","value_hex":storage::hex(&bytes),"value_length":bytes.len()})).await?;n=n.wrapping_add(1);
        }}
    }
    Ok(())
}
async fn record(
    cli: &Cli,
    c: Config,
    config_path: &Path,
    mode: ble::Mode,
    explicit: Option<bluer::Address>,
) -> Result<()> {
    let selected = if cli.demo {
        None
    } else {
        Some(address(&c, explicit)?)
    };
    let mut ui = if cli.workout {
        Some(tui::Ui::new()?)
    } else {
        None
    };
    let publisher = publish::Publisher::new(&c)?;
    let rec = Recorder::new(&base_dir(&c, config_path), cli.demo)?;
    let mut e = Engine {
        rec,
        publisher,
        output: if c.debug_stdout {
            Some(Output::new())
        } else {
            None
        },
        c: c.clone(),
        workout: Workout::default(),
        decoders: HashMap::new(),
        epoch: 0,
        raw: 0,
        unknown: 0,
        coverage_errors: 0,
        gaps: 0,
    };
    let start = Instant::now();
    let (tx, mut rx) = mpsc::channel(4096);
    let (stop_tx, stop_rx) = watch::channel(false);
    let out = ble::Events {
        tx,
        epoch: 0,
        start,
    };
    let mut worker = if cli.demo {
        tokio::spawn(demo(out, stop_rx))
    } else {
        tokio::spawn(ble::run(c.clone(), selected.unwrap(), mode, stop_rx, out))
    };
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut worker_result = None;
    let mut last_metrics = 0u64;
    let result:Result<()>=async{
        loop{
            tokio::select!{
                _=&mut signal=>break,
                row=rx.recv()=>match row{Some(row)=>{e.input(row,start.elapsed().as_millis() as u64)?;},None=>break},
                result=&mut worker=>{worker_result=Some(result);break;},
                _=tick.tick()=>{
                    let ms=start.elapsed().as_millis() as u64;e.workout.tick(ms,&c);
                    if cli.seconds.is_some_and(|s|start.elapsed()>=Duration::from_secs(s)){break;}
                    if ms/1000>last_metrics{last_metrics=ms/1000;let ch=c.metrics_channel.clone();e.emit(json!({"type":"metrics","workout":e.workout}),ms,&ch)?;}
                    if let Some(ui)=&mut ui{
                        match ui.input()?{tui::Action::Quit=>break,tui::Action::Pause=>{e.workout.set_paused(!e.workout.paused);let ch=c.status_channel.clone();e.emit(json!({"type":"pause","paused":e.workout.paused}),ms,&ch)?;},tui::Action::Marker=>{let ch=c.status_channel.clone();e.emit(json!({"type":"marker","label":"user marker"}),ms,&ch)?;},tui::Action::None=>{}}
                        let status=if let Some(p)=&e.publisher{p.stats().to_string()}else{"off".into()};
                        ui.draw(&e.workout,&c,cli.demo,e.raw,e.unknown,&status)?;
                    }
                }
            }
        }Ok(())
    }.await;
    let _ = stop_tx.send(true);
    if worker_result.is_none() {
        match timeout(Duration::from_secs(c.timeout_seconds + 8), &mut worker).await {
            Ok(r) => worker_result = Some(r),
            Err(_) => {
                worker.abort();
                worker_result = Some(worker.await);
            }
        }
    }
    let mut result = result;
    while let Ok(row) = rx.try_recv() {
        if let Err(err) = e.input(row, start.elapsed().as_millis() as u64) {
            result = Err(err);
            break;
        }
    }
    if let Some(r) = worker_result {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(err)) => result = Err(err),
            Err(_) => {
                result = Err(anyhow!(
                    "Bluetooth worker interrupted before cleanup completed"
                ))
            }
        }
    }
    let ms = start.elapsed().as_millis() as u64;
    e.workout.tick(ms, &c);
    let incomplete_frames = e.decoders.values().filter(|d| d.pending()).count();
    let status = if incomplete_frames > 0 || result.is_err() || e.coverage_errors > 0 || e.gaps > 0
    {
        "partial"
    } else {
        "complete_for_declared_scope"
    };
    let mut summary = json!({"type":"workout_summary","run_id":e.rec.run_id,"demo":cli.demo,"capture_status":status,"raw_values":e.raw,"undecoded_values":e.unknown,"coverage_errors":e.coverage_errors,"incomplete_frames":incomplete_frames,"connection_gaps":e.gaps,"workout":e.workout,"protocol_validation":"Sunny SF-B023005DGY startup and telemetry observed; extended scenarios remain untested"});
    let ch = c.workout_channel.clone();
    if let Err(err) = e.emit(summary.clone(), ms, &ch) {
        result = Err(err);
        summary["capture_status"] = json!("partial");
    }
    if let Some(p) = e.publisher.take() {
        summary["redis"] = p.finish().await;
    }
    if let Some(o) = e.output.take() {
        summary["stdout_dropped_or_failed"] = json!(o.finish());
    }
    let dir = e.rec.dir.clone();
    let save = e.rec.finish(summary);
    drop(ui);
    eprintln!("Saved capture and workout: {}", dir.display());
    result.and(save)?;
    if matches!(mode, ble::Mode::Pair | ble::Mode::Connect) && !cli.demo {
        // Persist only the selection; do not persist any transient secret CLI overrides.
        let mut saved = Config::load(config_path)?;
        saved.device = selected.map(|a| a.to_string());
        if c.adapter.is_some() {
            saved.adapter = c.adapter;
        }
        config::write_private(config_path, toml::to_string_pretty(&saved)?.as_bytes())?;
        eprintln!(
            "Bike selection saved. {}",
            if mode == ble::Mode::Pair {
                "Bluetooth bond established."
            } else {
                "GATT connection verified; no bond requested."
            }
        );
    }
    Ok(())
}
async fn replay(c: &Config, path: &Path) -> Result<()> {
    let mut decoders: HashMap<String, Decoder> = HashMap::new();
    let mut publisher = publish::Publisher::new(c)?;
    let mut epoch = 0;
    for line in std::io::BufReader::new(fs::File::open(path)?).lines() {
        let row: Value = serde_json::from_str(&line?)?;
        if row["epoch"].as_u64().unwrap_or(epoch) != epoch {
            epoch = row["epoch"].as_u64().unwrap();
            decoders.clear();
        }
        if let Some(p) = &mut publisher {
            let kind = row["type"].as_str().unwrap_or("");
            if !matches!(kind, "decoded" | "metrics" | "workout_summary") {
                p.replay(
                    if matches!(kind, "connection" | "retry" | "coverage_gap" | "ready") {
                        &c.status_channel
                    } else {
                        &c.raw_channel
                    },
                    &row,
                )
                .await?;
            }
        }
        if matches!(
            row["type"].as_str(),
            Some("notification" | "gatt_value_update" | "read_result")
        ) {
            let uuid = row["characteristic_uuid"].as_str().unwrap_or("");
            let key = format!("{}:{}:{uuid}", row["service_id"], row["characteristic_id"]);
            let data = unhex(row["value_hex"].as_str().unwrap_or(""))?;
            let ms = row["observed_elapsed_ms"]
                .as_u64()
                .or(row["elapsed_ms"].as_u64())
                .unwrap_or(0);
            match decoders.entry(key).or_default().decode_event(
                uuid,
                &data,
                ms,
                c,
                row["sequence"].as_u64().unwrap_or(0),
            ) {
                Ok(Some(s)) => {
                    let decoded = json!({"type":"decoded","run_id":row["run_id"],"source_event":row["sequence"],"sample":s});
                    println!("{decoded}");
                    if let Some(p) = &mut publisher {
                        p.replay(&c.metrics_channel, &decoded).await?;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    eprintln!("Skipping malformed measurement at a recorded event");
                }
            }
        }
    }
    if let Some(p) = publisher {
        let stats = p.finish().await;
        eprintln!("Redis replay: {stats}");
        if stats["failed_or_unconfirmed"].as_u64().unwrap_or(0)
            + stats["pending"].as_u64().unwrap_or(0)
            + stats["queue_dropped"].as_u64().unwrap_or(0)
            > 0
        {
            bail!("Redis replay incomplete; original local data is retained");
        }
    }
    Ok(())
}
async fn app() -> Result<()> {
    // Even --help / --version bootstrap a private default config on first start.
    let default = config::init()?;
    let cli = Cli::parse();
    let path = cli.config.as_ref().unwrap_or(&default);
    if cli.workout && !matches!(cli.command, None | Some(Command::Capture { .. })) {
        bail!("--workout can be used alone or with capture");
    }
    if cli.demo && !matches!(cli.command, None | Some(Command::Capture { .. })) {
        bail!("--demo can be used alone or with capture");
    }
    let mut c = Config::load(path)?;
    c.apply(&cli.settings)?;
    match &cli.command {
        Some(Command::Config) => {
            println!("{}", path.display());
            Ok(())
        }
        Some(Command::Doctor) => ble::doctor(&c).await,
        Some(Command::Scan) => {
            tokio::select! {r=ble::scan(&c,cli.seconds.unwrap_or(15))=>r,_=shutdown_signal()=>Ok(())}
        }
        Some(Command::History) => {
            let base = base_dir(&c, path);
            let mut paths = fs::read_dir(base)?
                .filter_map(|e| e.ok().map(|e| e.path().join("summary.json")))
                .filter(|p| p.is_file())
                .collect::<Vec<_>>();
            paths.sort();
            for p in paths {
                let v: Value = serde_json::from_slice(&fs::read(p)?)?;
                println!("{v}");
            }
            Ok(())
        }
        Some(Command::Replay { path: input }) => replay(&c, input).await,
        Some(Command::Forget { address: a }) => {
            let a = address(&c, *a)?;
            ble::manage(&c, a, true).await?;
            let mut saved = Config::load(path)?;
            if saved
                .device
                .as_deref()
                .and_then(|s| s.parse::<bluer::Address>().ok())
                == Some(a)
            {
                saved.device = None;
                config::write_private(path, toml::to_string_pretty(&saved)?.as_bytes())?;
            }
            eprintln!("Bike bond removed.");
            Ok(())
        }
        Some(Command::Disconnect { address: a }) => {
            ble::manage(&c, address(&c, *a)?, false).await?;
            eprintln!("Bike disconnected.");
            Ok(())
        }
        Some(Command::Inspect { address }) => {
            record(&cli, c, path, ble::Mode::Inspect, *address).await
        }
        Some(Command::Connect { address }) => {
            record(&cli, c, path, ble::Mode::Connect, *address).await
        }
        Some(Command::Pair { address }) => record(&cli, c, path, ble::Mode::Pair, *address).await,
        Some(Command::Capture { address }) => {
            record(&cli, c, path, ble::Mode::Capture, *address).await
        }
        None => record(&cli, c, path, ble::Mode::Capture, None).await,
    }
}
#[tokio::main]
async fn main() {
    if let Err(e) = app().await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
