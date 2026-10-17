mod ble;
mod config;
mod export;
mod food;
mod metrics;
mod profiles;
mod publish;
mod service;
mod sinks;
mod storage;
mod strava;
mod sunny;
mod training;
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
use tokio::sync::{mpsc, watch};

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
    rec: Option<Recorder>,
    run_id: String,
    sequence: u64,
    demo: bool,
    databases: Vec<sinks::Sink>,
    external_hr: Option<(metrics::Sample, u64)>,
    detected_profile: String,
    publisher: Option<publish::Publisher>,
    output: Option<Output>,
    c: Config,
    workout: Workout,
    decoders: HashMap<String, Decoder>,
    epochs: HashMap<String, u64>,
    raw: u64,
    unknown: u64,
    coverage_errors: u64,
    gaps: u64,
}
impl Engine {
    fn emit(&mut self, mut row: Value, ms: u64, channel: &str) -> Result<Value> {
        self.sequence += 1;
        row["schema"] = json!("sunnyfit.event.v1");
        row["run_id"] = json!(self.run_id);
        row["sequence"] = json!(self.sequence);
        row["timestamp_unix_ms"] = json!(storage::unix_ms());
        row["elapsed_ms"] = json!(ms);
        row["demo"] = json!(self.demo);
        if let Some(rec) = &mut self.rec {
            rec.record(&row)?;
        }
        for sink in &mut self.databases {
            sink.enqueue(&row);
        }
        if matches!(
            row["type"].as_str(),
            Some("connection" | "retry" | "ready" | "workout_summary")
        ) {
            service::log(
                row["type"].as_str().unwrap(),
                json!({"run_id":self.run_id,"role":row["device_role"],"state":row["state"],"elapsed_ms":ms}),
            );
        }
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
        let role = row["device_role"].as_str().unwrap_or("bike").to_owned();
        let hr = role == "heart_rate";
        if self.epochs.get(&role) != Some(&epoch) {
            self.decoders
                .retain(|key, _| !key.starts_with(&format!("{role}:")));
            self.epochs.insert(role.clone(), epoch);
        }
        let kind = row["type"].as_str().unwrap_or("").to_owned();
        if !hr {
            let u = row["characteristic_uuid"].as_str().unwrap_or("");
            if u == sunny::MOTION {
                self.detected_profile = "sunny-mini".into();
            } else if self.detected_profile != "sunny-mini"
                && u == "00002ad2-0000-1000-8000-00805f9b34fb"
            {
                self.detected_profile = "ftms".into();
            } else if matches!(self.detected_profile.as_str(), "raw" | "heart-rate")
                && u == "00002a5b-0000-1000-8000-00805f9b34fb"
            {
                self.detected_profile = "csc".into();
            } else if self.detected_profile == "raw" && u == profiles::HEART_RATE {
                self.detected_profile = "heart-rate".into();
            }
        }

        if kind == "connection" {
            if hr {
                if row["state"] != "ready" {
                    self.external_hr = None;
                    self.workout
                        .external_heart_rate(&metrics::Sample::default(), ms);
                }
            } else {
                self.workout
                    .connection(row["state"].as_str().unwrap_or("unknown"));
            }
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
                "{role}:{}:{}:{}",
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
                    let decoded = json!({"type":"decoded","device_role":role,"source_event":row["sequence"],"sample":sample});
                    if hr {
                        if uuid == profiles::HEART_RATE {
                            self.workout.external_heart_rate(&sample, ms);
                            self.external_hr = Some((sample, ms));
                        }
                    } else {
                        let mut sample = sample;
                        if self.c.heart_rate_device.is_some() {
                            sample.heart_rate_bpm = None;
                        }
                        self.workout
                            .ingest(sample, ms, row["sequence"].as_u64().unwrap(), &self.c);
                    }
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
    mut c: Config,
    config_path: &Path,
    mode: ble::Mode,
    explicit: Option<bluer::Address>,
) -> Result<()> {
    let selected = if cli.demo {
        None
    } else {
        Some(address(&c, explicit)?)
    };
    c.device = selected.map(|a| a.to_string());
    let mut ui = if cli.workout {
        Some(tui::Ui::new()?)
    } else {
        None
    };
    let publisher = publish::Publisher::new(&c)?;
    let rec = if c.sinks.iter().any(|s| s == "local") {
        Some(Recorder::new(&base_dir(&c, config_path), cli.demo)?)
    } else {
        None
    };
    let run_id = rec
        .as_ref()
        .map(|r| r.run_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let databases = sinks::Sink::start(&c)?;
    let mut training = if cli.workout {
        Some(training::Training::new(
            &c,
            config_path.parent().unwrap_or(Path::new(".")),
            &run_id,
            cli.demo,
            &base_dir(&c, config_path),
        )?)
    } else {
        None
    };
    let mut e = Engine {
        rec,
        run_id,
        sequence: 0,
        demo: cli.demo,
        databases,
        external_hr: None,
        detected_profile: "raw".into(),
        publisher,
        output: if c.debug_stdout {
            Some(Output::new())
        } else {
            None
        },
        c: c.clone(),
        workout: Workout::default(),
        decoders: HashMap::new(),
        epochs: HashMap::new(),
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
        role: "bike".into(),
        start,
    };
    let mut hr_worker = if !cli.demo && mode == ble::Mode::Capture {
        if let Some(hr) = &c.heart_rate_device {
            let mut hc = c.clone();
            hc.device = Some(hr.clone());
            hc.adapter = c.heart_rate_adapter.clone().or(c.adapter.clone());
            hc.profile = "heart-rate".into();
            hc.loaded_profile = None;
            hc.sunny_protocol = false;
            hc.init_command.clear();
            hc.pair = false;
            let mut ho = out.clone();
            ho.role = "heart_rate".into();
            Some(tokio::spawn(ble::run(
                hc,
                hr.parse()?,
                ble::Mode::Capture,
                stop_rx.clone(),
                ho,
            )))
        } else {
            None
        }
    } else {
        None
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
                    if let Some((sample,time))=&e.external_hr {if ms.saturating_sub(*time)<=c.stale_seconds*1000 {e.workout.external_heart_rate(sample,*time);}}
                    if last_metrics/30<ms/30000 {service::log("storage_health",json!({"databases":e.databases.iter().map(|s|s.stats()).collect::<Vec<_>>(),"redis":e.publisher.as_ref().map(|p|p.stats())}));}
                    if cli.seconds.is_some_and(|s|start.elapsed()>=Duration::from_secs(s)){break;}
                    if ms/1000>last_metrics{last_metrics=ms/1000;let ch=c.metrics_channel.clone();e.emit(json!({"type":"metrics","workout":e.workout}),ms,&ch)?;}
                    if let Some(ui)=&mut ui{
                        match ui.input()?{tui::Action::Quit=>break,tui::Action::Pause=>{e.workout.set_paused(!e.workout.paused);let ch=c.status_channel.clone();e.emit(json!({"type":"pause","paused":e.workout.paused}),ms,&ch)?;},tui::Action::Marker=>{let ch=c.status_channel.clone();e.emit(json!({"type":"marker","label":"user marker"}),ms,&ch)?;},tui::Action::Focus=>{if let Some(t)=&mut training {t.focus_paused = !t.focus_paused;}},tui::Action::ResetFocus=>{if let Some(t)=&mut training {t.focus_s=0.;}},tui::Action::None=>{}}
                        if let Some(t)=&mut training {t.tick(&e.workout,&c,ms)?;ui.draw(&e.workout,&c,cli.demo,t)?;}
                    }
                }
            }
        }Ok(())
    }.await;
    let _ = stop_tx.send(true);
    if worker_result.is_none() {
        let deadline = tokio::time::sleep(Duration::from_secs(c.timeout_seconds + 8));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                r=&mut worker=>{worker_result=Some(r);break;},
                row=rx.recv()=>{if let Some(row)=row {let _=e.input(row,start.elapsed().as_millis() as u64);}},
                _=&mut deadline=>{worker.abort();worker_result=Some(worker.await);break;}
            }
        }
    }
    if let Some(mut task) = hr_worker.take() {
        let deadline = tokio::time::sleep(Duration::from_secs(c.timeout_seconds + 8));
        tokio::pin!(deadline);
        loop {
            tokio::select! { _=&mut task=>break,
                row=rx.recv()=>{if let Some(row)=row {let _=e.input(row,start.elapsed().as_millis() as u64);}},
                _=&mut deadline=>{task.abort();let _=task.await;break;}
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
    let mut summary = json!({"type":"workout_summary","run_id":e.run_id,"demo":cli.demo,"capture_status":status,"raw_values":e.raw,"undecoded_values":e.unknown,"coverage_errors":e.coverage_errors,"incomplete_frames":incomplete_frames,"connection_gaps":e.gaps,"workout":e.workout,"protocol_validation":"Sunny SF-B023005DGY startup and telemetry observed; extended scenarios remain untested"});
    summary["food_equivalent"] = json!(food::message(e.workout.calories_kcal));
    if let Some(t) = &mut training {
        if let Err(err) = t.persist() {
            result = Err(err);
        }
    }
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
    let mut database_stats = Vec::new();
    for sink in e.databases {
        database_stats.push(sink.finish().await);
    }
    summary["databases"] = json!(database_stats);
    service::log(
        "stopped",
        json!({"databases":summary["databases"],"redis":summary["redis"]}),
    );
    drop(ui);
    if let Some(rec) = e.rec {
        let dir = rec.dir.clone();
        let save = rec.finish(summary);
        eprintln!("Saved capture and workout: {}", dir.display());
        result.and(save)?;
    } else {
        result?;
    }

    if matches!(cli.command, Some(Command::Wizard { .. })) {
        let mut saved = Config::load(config_path)?;
        if e.detected_profile == "heart-rate" {
            saved.heart_rate_device = selected.map(|a| a.to_string());
            saved.heart_rate_adapter = c.adapter.clone();
        } else {
            saved.device = selected.map(|a| a.to_string());
            saved.adapter = c.adapter.clone();
            saved.profile = e.detected_profile.clone();
        }
        config::write_private(config_path, toml::to_string_pretty(&saved)?.as_bytes())?;
        eprintln!("Discovery complete. Saved {} profile. GATT inspection is in the capture above. Use validate-profile with sanitized fixtures before sharing a new decoder.",e.detected_profile);
    }
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
    food::init(
        c.food_file
            .as_deref()
            .unwrap_or(&default.parent().unwrap().join("callories-burn.json")),
    )?;
    if c.systemd {
        if cli.workout {
            bail!("--systemd and --workout cannot be combined");
        }
        service::init(&c.log_file)?;
    }
    match &cli.command {
        Some(Command::Profiles) => {
            profiles::list();
            Ok(())
        }
        Some(Command::ValidateProfile { fixture }) => {
            println!("Validated {} frames", profiles::validate(&c, fixture)?);
            Ok(())
        }
        Some(Command::Sessions { database }) => {
            for row in sinks::sessions(&c, database).await? {
                println!("{row}");
            }
            Ok(())
        }
        Some(Command::Wizard { address }) => {
            let selected = if let Some(a) = address {
                *a
            } else {
                ble::choose(&c, cli.seconds.unwrap_or(15)).await?
            };
            c.reconnect = false;
            c.sinks = vec!["local".into()];
            record(&cli, c, path, ble::Mode::Inspect, Some(selected)).await
        }
        Some(Command::Export {
            input,
            format,
            output,
            session,
            database,
        }) => {
            let rows = match (input, session) {
                (Some(path), None) => export::read(path)?,
                (None, Some(id)) => sinks::events(&c, database, id).await?,
                _ => bail!("provide either INPUT or --session ID"),
            };
            let activity = export::Activity::from_rows(&rows)?;
            export::save(output, &activity.encode(format)?)?;
            println!("Exported indoor cycling activity to {}", output.display());
            Ok(())
        }
        Some(Command::Sync { input, name }) => {
            strava::sync(
                path.parent().unwrap_or(Path::new(".")),
                input,
                name.as_deref(),
            )
            .await
        }
        Some(Command::StravaAuth { exchange }) => {
            strava::auth(path.parent().unwrap_or(Path::new(".")), *exchange).await
        }
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
        service::log(
            "fatal",
            json!({"message":"application stopped; see journal for diagnostic"}),
        );
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heart_rate_disconnect_does_not_reset_bike() {
        let mut e = Engine {
            rec: None,
            run_id: "test".into(),
            sequence: 0,
            demo: false,
            databases: Vec::new(),
            external_hr: None,
            detected_profile: "raw".into(),
            publisher: None,
            output: None,
            c: Config::default(),
            workout: Workout::default(),
            decoders: HashMap::new(),
            epochs: HashMap::new(),
            raw: 0,
            unknown: 0,
            coverage_errors: 0,
            gaps: 0,
        };
        e.input(
            json!({"type":"connection","state":"ready","device_role":"bike","epoch":1}),
            0,
        )
        .unwrap();
        e.input(json!({"type":"notification","device_role":"bike","epoch":1,"characteristic_uuid":"00002ad2-0000-1000-8000-00805f9b34fb","value_hex":"0400d0077800"}),100).unwrap();
        e.input(json!({"type":"notification","device_role":"heart_rate","epoch":7,"characteristic_uuid":profiles::HEART_RATE,"value_hex":"0648"}),200).unwrap();
        assert_eq!(e.workout.current.heart_rate_bpm, Some(72.));
        assert_eq!(e.workout.current.cadence_rpm, Some(60.));
        e.input(json!({"type":"connection","state":"disconnected","device_role":"heart_rate","epoch":7}),300).unwrap();
        assert_eq!(e.workout.current.heart_rate_bpm, None);
        assert_eq!(e.workout.current.cadence_rpm, Some(60.));
        assert_eq!(e.workout.connection, "ready");
        e.input(json!({"type":"notification","device_role":"heart_rate","epoch":8,"characteristic_uuid":profiles::HEART_RATE,"value_hex":"014b00"}),400).unwrap();
        assert_eq!(e.workout.current.heart_rate_bpm, Some(75.));
        assert_eq!(e.decoders.len(), 2);
    }
}
