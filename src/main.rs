use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use bluer::{
    gatt::remote::Characteristic, Adapter, AdapterEvent, Address, Device, DeviceEvent,
    DeviceProperty, DiscoveryFilter, DiscoveryTransport, Session,
};
use clap::{Args, Parser, Subcommand};
use futures::{stream, stream::BoxStream, stream::SelectAll, StreamExt};
use redis::AsyncCommands;
use serde_json::{json, Value};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = "Inspect a Linux BlueZ BLE device and capture raw GATT value updates.\nNo SunnyFit decoder or proprietary initialization is assumed. JSONL goes to stdout; diagnostics go to stderr."
)]
struct Cli {
    /// BlueZ adapter name, e.g. hci0; otherwise use the default adapter.
    #[arg(long, global = true)]
    adapter: Option<String>,
    /// Timeout for each Bluetooth setup/read operation.
    #[arg(long, global = true, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..))]
    timeout_seconds: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover LE devices; cached entries are not evidence that a device is nearby.
    Scan {
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
        seconds: u64,
    },
    /// Connect and list service, characteristic, descriptor UUIDs and local IDs.
    Inspect {
        #[command(flatten)]
        device: DeviceArgs,
        /// Read readable characteristic values. Does not subscribe or write.
        #[arg(long)]
        read: bool,
    },
    /// Subscribe to value updates and optionally publish the same JSON to Redis.
    Capture {
        #[command(flatten)]
        device: DeviceArgs,
        /// Capture duration AFTER subscriptions are ready; omit to run until Ctrl-C.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        seconds: Option<u64>,
        /// Select decimal SERVICE_ID:CHAR_ID from inspect; repeat to select several.
        /// Omit to select all characteristics offering notify or indicate.
        #[arg(long, value_parser = parse_selection)]
        select: Vec<(u16, u16)>,
        /// Redis URL. Prefer SUNNY_REDIS_URL to keep credentials out of shell history.
        #[arg(long, env = "SUNNY_REDIS_URL", hide_env_values = true)]
        redis_url: Option<String>,
        #[arg(long, default_value = "sunny:bike:raw")]
        channel: String,
    },
}

#[derive(Args)]
struct DeviceArgs {
    /// Bluetooth address shown by scan, e.g. AA:BB:CC:DD:EE:FF.
    address: Address,
    /// Opt in to pairing/bonding using a NoInputNoOutput agent; never sets trust.
    #[arg(long)]
    pair: bool,
}

fn parse_selection(value: &str) -> std::result::Result<(u16, u16), String> {
    let (service, characteristic) = value
        .split_once(':')
        .ok_or("use decimal SERVICE_ID:CHAR_ID, e.g. 12:14")?;
    let parse = |s: &str| {
        s.parse::<u16>()
            .map_err(|_| "IDs must be decimal u16 values".to_owned())
    };
    Ok((parse(service)?, parse(characteristic)?))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

fn unix_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

fn emit(value: &Value) -> Result<String> {
    let line = serde_json::to_string(value)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}").context("writing JSONL to stdout")?;
    stdout.flush().context("flushing JSONL to stdout")?;
    Ok(line)
}

async fn bounded<T>(
    limit: Duration,
    label: &str,
    operation: impl Future<Output = bluer::Result<T>>,
) -> Result<T> {
    timeout(limit, operation)
        .await
        .with_context(|| format!("{label} timed out"))?
        .with_context(|| label.to_owned())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let limit = Duration::from_secs(cli.timeout_seconds);
    let session = bounded(limit, "opening BlueZ session", Session::new()).await?;
    let adapter = if let Some(name) = cli.adapter.as_deref() {
        session.adapter(name)?
    } else {
        bounded(
            limit,
            "finding default Bluetooth adapter",
            session.default_adapter(),
        )
        .await?
    };
    if !bounded(limit, "checking adapter power", adapter.is_powered()).await? {
        bail!(
            "adapter {} is powered off; enable Bluetooth and retry",
            adapter.name()
        );
    }
    match &cli.command {
        Command::Scan { seconds } => {
            tokio::select! {
                result = scan(&adapter, *seconds, limit) => result,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("listening for Ctrl-C")?;
                    eprintln!("Scan interrupted.");
                    Ok(())
                }
            }
        }
        Command::Inspect { device, .. } | Command::Capture { device, .. } => {
            with_device(&session, &adapter, device, &cli.command, limit).await
        }
    }
}

async fn scan(adapter: &Adapter, seconds: u64, limit: Duration) -> Result<()> {
    let cached: HashSet<Address> =
        bounded(limit, "listing cached devices", adapter.device_addresses())
            .await?
            .into_iter()
            .collect();
    let mut snapshots = HashMap::new();
    eprintln!(
        "Scanning {} for {seconds}s. Cached addresses and RSSI may be stale; wake/pedal the bike.",
        adapter.name()
    );
    for &address in &cached {
        scan_record(adapter, address, "cached", true, &mut snapshots, limit).await?;
    }
    bounded(
        limit,
        "setting LE discovery filter",
        adapter.set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            ..Default::default()
        }),
    )
    .await?;
    let events = bounded(
        limit,
        "starting discovery",
        adapter.discover_devices_with_changes(),
    )
    .await?;
    futures::pin_mut!(events);
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            event = events.next() => match event {
                Some(AdapterEvent::DeviceAdded(address)) => {
                    scan_record(adapter, address, "discovery_or_property_change", cached.contains(&address), &mut snapshots, limit).await?;
                }
                Some(_) => {},
                None => bail!("Bluetooth discovery stream ended unexpectedly"),
            }
        }
    }
    Ok(()) // Dropping the discovery stream stops this client's discovery session.
}

async fn scan_record(
    adapter: &Adapter,
    address: Address,
    source: &str,
    cached: bool,
    snapshots: &mut HashMap<Address, Value>,
    limit: Duration,
) -> Result<()> {
    let device = adapter.device(address)?;
    let values = timeout(limit, async {
        tokio::try_join!(
            device.name(),
            device.rssi(),
            device.manufacturer_data(),
            device.service_data(),
            device.uuids()
        )
    })
    .await;
    let (name, rssi, manufacturer, service_data, uuids) = match values {
        Ok(Ok(values)) => values,
        other => {
            eprintln!("Skipping unavailable device {address}: {other:?}");
            return Ok(());
        }
    };
    let manufacturer: BTreeMap<_, _> = manufacturer
        .unwrap_or_default()
        .into_iter()
        .map(|(id, bytes)| (format!("{id:04x}"), hex(&bytes)))
        .collect();
    let service_data: BTreeMap<_, _> = service_data
        .unwrap_or_default()
        .into_iter()
        .map(|(id, bytes)| (id.to_string(), hex(&bytes)))
        .collect();
    let mut uuids: Vec<_> = uuids
        .unwrap_or_default()
        .into_iter()
        .map(|u| u.to_string())
        .collect();
    uuids.sort();
    let snapshot = json!({"name":name,"rssi":rssi,"manufacturer_data_hex":manufacturer,
        "service_data_hex":service_data,"service_uuids":uuids});
    if snapshots.get(&address) == Some(&snapshot) {
        return Ok(());
    }
    snapshots.insert(address, snapshot.clone());
    emit(
        &json!({"type":"device","timestamp_unix_ms":unix_ms()?,"adapter":adapter.name(),
        "address":address.to_string(),"source":source,"cached_at_start":cached,"properties":snapshot}),
    )?;
    Ok(())
}

async fn with_device(
    session: &Session,
    adapter: &Adapter,
    args: &DeviceArgs,
    command: &Command,
    limit: Duration,
) -> Result<()> {
    let device = adapter.device(args.address)?;
    let was_connected = bounded(limit, "checking connection", device.is_connected()).await?;
    let mut publisher = match command {
        Command::Capture {
            redis_url: Some(url),
            channel,
            ..
        } => Some(Publisher::new(url, channel.clone())?),
        _ => None,
    };
    // Keep the agent alive for the whole operation, including cancellation/cleanup.
    let mut agent = None;
    let operation = async {
        if args.pair && !bounded(limit, "checking pairing", device.is_paired()).await? {
            agent = Some(
                bounded(
                    limit,
                    "registering pairing agent",
                    session.register_agent(bluer::agent::Agent::default()),
                )
                .await?,
            );
            bounded(
                limit,
                "pairing (for PIN/passkey devices use bluetoothctl)",
                device.pair(),
            )
            .await?;
        }
        if !bounded(limit, "checking connection", device.is_connected()).await? {
            bounded(
                limit,
                "connecting; scan first if BlueZ does not know this address",
                device.connect(),
            )
            .await?;
        }
        match command {
            Command::Inspect { read, .. } => inspect(adapter, &device, *read, limit).await,
            Command::Capture {
                seconds, select, ..
            } => {
                capture(
                    adapter,
                    &device,
                    *seconds,
                    select,
                    publisher.as_mut(),
                    limit,
                )
                .await
            }
            Command::Scan { .. } => unreachable!(),
        }
    };
    let result = tokio::select! {
        result = operation => result,
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => { eprintln!("Interrupted; stopping subscriptions and cleaning up."); Ok(()) },
                Err(error) => Err(error).context("listening for Ctrl-C"),
            }
        }
    };
    // Drop cancels Pair; Disconnect also cancels an in-flight Connect. Do this on errors too.
    let cleanup = if !was_connected {
        bounded(limit, "disconnecting device", device.disconnect()).await
    } else {
        eprintln!("Leaving the pre-existing Bluetooth connection connected.");
        Ok(())
    };
    drop(agent);
    let forwarding = if let Some(publisher) = publisher {
        publisher.finish().await
    } else {
        Ok(())
    };
    for error in [&cleanup, &forwarding]
        .into_iter()
        .filter_map(|r| r.as_ref().err())
    {
        eprintln!("Cleanup/forwarding: {error:#}");
    }
    result.and(cleanup).and(forwarding)
}

async fn inspect(adapter: &Adapter, device: &Device, read: bool, limit: Duration) -> Result<()> {
    let mut services = bounded(limit, "resolving GATT services", device.services()).await?;
    services.sort_by_key(|s| s.id());
    eprintln!("GATT IDs below are decimal BlueZ instance IDs; re-inspect after rediscovery. No notification subscriptions are active.");
    for service in services {
        let service_uuid = bounded(limit, "reading service UUID", service.uuid())
            .await?
            .to_string();
        emit(
            &json!({"type":"service","adapter":adapter.name(),"address":device.address().to_string(),
            "service_id":service.id(),"service_uuid":service_uuid}),
        )?;
        let mut characteristics =
            bounded(limit, "listing characteristics", service.characteristics()).await?;
        characteristics.sort_by_key(|c| c.id());
        for characteristic in characteristics {
            let uuid = bounded(limit, "reading characteristic UUID", characteristic.uuid())
                .await?
                .to_string();
            let flags = bounded(
                limit,
                "reading characteristic flags",
                characteristic.flags(),
            )
            .await?;
            let mut row = json!({"type":"characteristic","service_id":service.id(),"service_uuid":service_uuid,
                "characteristic_id":characteristic.id(),"characteristic_uuid":uuid,"properties":flags});
            if read && flags.read {
                match bounded(limit, "reading characteristic value", characteristic.read()).await {
                    Ok(bytes) => row["read_hex"] = json!(hex(&bytes)),
                    Err(error) => row["read_error"] = json!(format!("{error:#}")),
                }
            }
            emit(&row)?;
            let mut descriptors =
                bounded(limit, "listing descriptors", characteristic.descriptors()).await?;
            descriptors.sort_by_key(|d| d.id());
            for descriptor in descriptors {
                emit(
                    &json!({"type":"descriptor","service_id":service.id(),"characteristic_id":characteristic.id(),
                    "descriptor_id":descriptor.id(),"descriptor_uuid":bounded(limit,"reading descriptor UUID",descriptor.uuid()).await?.to_string()}),
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Source {
    service_id: u16,
    service_uuid: String,
    characteristic_id: u16,
    characteristic_uuid: String,
}

enum Update {
    Value(Source, Vec<u8>),
    Closed(Source),
}

async fn capture(
    adapter: &Adapter,
    device: &Device,
    seconds: Option<u64>,
    selected: &[(u16, u16)],
    mut publisher: Option<&mut Publisher>,
    limit: Duration,
) -> Result<()> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let start = Instant::now();
    let mut device_events = bounded(limit, "watching connection state", device.events())
        .await?
        .boxed();
    if !bounded(limit, "checking connection state", device.is_connected()).await? {
        bail!("device disconnected before capture");
    }
    let mut candidates: Vec<(Source, Characteristic)> = Vec::new();
    let requested: HashSet<_> = selected.iter().copied().collect();
    for service in bounded(limit, "resolving GATT services", device.services()).await? {
        let service_uuid = bounded(limit, "reading service UUID", service.uuid())
            .await?
            .to_string();
        for characteristic in
            bounded(limit, "listing characteristics", service.characteristics()).await?
        {
            let key = (service.id(), characteristic.id());
            if !requested.is_empty() && !requested.contains(&key) {
                continue;
            }
            let flags = bounded(
                limit,
                "reading characteristic flags",
                characteristic.flags(),
            )
            .await?;
            if !flags.notify && !flags.indicate {
                if requested.contains(&key) {
                    bail!(
                        "selected characteristic {}:{} does not offer notify or indicate",
                        key.0,
                        key.1
                    );
                }
                continue;
            }
            candidates.push((
                Source {
                    service_id: key.0,
                    service_uuid: service_uuid.clone(),
                    characteristic_id: key.1,
                    characteristic_uuid: bounded(
                        limit,
                        "reading characteristic UUID",
                        characteristic.uuid(),
                    )
                    .await?
                    .to_string(),
                },
                characteristic,
            ));
        }
    }
    for &(service, characteristic) in &requested {
        if !candidates
            .iter()
            .any(|(s, _)| s.service_id == service && s.characteristic_id == characteristic)
        {
            bail!(
                "selected characteristic {service}:{characteristic} was not found; inspect again"
            );
        }
    }
    if candidates.is_empty() {
        bail!("no notify/indicate characteristics found; save the inspect output for analysis");
    }
    candidates.sort_by_key(|(s, _)| (s.service_id, s.characteristic_id));
    let mut streams: SelectAll<BoxStream<'static, Update>> = SelectAll::new();
    let mut skipped = 0usize;
    for (source, characteristic) in candidates {
        let notifications = match bounded(
            limit,
            "subscribing to characteristic",
            characteristic.notify(),
        )
        .await
        {
            Ok(notifications) => notifications,
            Err(error) => {
                let error = error.context(format!(
                    "characteristic {}:{} ({})",
                    source.service_id, source.characteristic_id, source.characteristic_uuid
                ));
                if !requested.is_empty() {
                    return Err(error);
                }
                eprintln!("Skipping failed subscription: {error:#}");
                skipped += 1;
                continue;
            }
        };
        eprintln!(
            "Subscribed {}:{} service={} characteristic={}",
            source.service_id,
            source.characteristic_id,
            source.service_uuid,
            source.characteristic_uuid
        );
        let end_source = source.clone();
        streams.push(
            notifications
                .map(move |bytes| Update::Value(source.clone(), bytes))
                .chain(stream::once(async move { Update::Closed(end_source) }))
                .boxed(),
        );
    }
    if streams.is_empty() {
        bail!(
            "all {skipped} candidate notification subscriptions failed; inspect the errors above"
        );
    }
    if skipped > 0 {
        eprintln!(
            "Partial capture: {skipped} characteristic subscriptions failed; {} succeeded.",
            streams.len()
        );
    }
    eprintln!("Ready; session {session_id}. Pedal the bike now. No value reads or application writes are issued during capture.");
    let deadline = async {
        if let Some(seconds) = seconds {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
        } else {
            futures::future::pending::<()>().await;
        }
    };
    tokio::pin!(deadline);
    let mut sequence = 0u64;
    loop {
        tokio::select! {
            _ = &mut deadline => { eprintln!("Capture duration complete; {sequence} value updates recorded."); break; }
            event = device_events.next() => match event {
                Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) => bail!("bike disconnected unexpectedly after {sequence} updates"),
                None => bail!("device event stream ended unexpectedly after {sequence} updates"),
                _ => {},
            },
            event = streams.next() => match event {
                Some(Update::Value(source, bytes)) => {
                    sequence += 1;
                    let row = json!({"schema":"sunny-ble.raw.v1","type":"gatt_value_update",
                        "session_id":session_id,"sequence":sequence,"timestamp_unix_ms":unix_ms()?,
                        "elapsed_ms":start.elapsed().as_millis(),"adapter":adapter.name(),"address":device.address().to_string(),
                        "service_id":source.service_id,"service_uuid":source.service_uuid,
                        "characteristic_id":source.characteristic_id,"characteristic_uuid":source.characteristic_uuid,
                        "value_hex":hex(&bytes),"value_length":bytes.len()});
                    let line = emit(&row)?; // Stdout is authoritative: enqueue only after the line is written.
                    if let Some(publisher) = publisher.as_mut() { publisher.enqueue(line); }
                }
                Some(Update::Closed(source)) => bail!("notification stream {}:{} ended unexpectedly",source.service_id,source.characteristic_id),
                None => bail!("all notification streams ended unexpectedly"),
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct RedisStats {
    published: AtomicU64,
    failed: AtomicU64,
    unavailable: AtomicBool,
}

struct Publisher {
    sender: Option<mpsc::Sender<String>>,
    worker: JoinHandle<()>,
    stats: Arc<RedisStats>,
    enqueued: u64,
    dropped: u64,
}

impl Publisher {
    fn new(url: &str, channel: String) -> Result<Self> {
        // Do not include a Redis URL or errors that could contain its credentials in diagnostics.
        let client = redis::Client::open(url)
            .map_err(|_| anyhow!("invalid Redis URL; check SUNNY_REDIS_URL"))?;
        let (sender, mut receiver) = mpsc::channel::<String>(512);
        let stats = Arc::new(RedisStats::default());
        let worker_stats = stats.clone();
        let worker = tokio::spawn(async move {
            let mut connection = match timeout(
                Duration::from_secs(5),
                client.get_multiplexed_async_connection(),
            )
            .await
            {
                Ok(Ok(connection)) => Some(connection),
                _ => {
                    worker_stats.unavailable.store(true, Ordering::Relaxed);
                    eprintln!("Redis connection failed/timed out. Forwarding is disabled for this capture; stdout continues.");
                    None
                }
            };
            while let Some(line) = receiver.recv().await {
                let successful = if let Some(connection) = &mut connection {
                    let publish = async {
                        let result: redis::RedisResult<u64> =
                            connection.publish(&channel, &line).await;
                        result
                    };
                    matches!(timeout(Duration::from_secs(2), publish).await, Ok(Ok(_)))
                } else {
                    false
                };
                if successful {
                    worker_stats.published.fetch_add(1, Ordering::Relaxed);
                } else {
                    worker_stats.failed.fetch_add(1, Ordering::Relaxed);
                    worker_stats.unavailable.store(true, Ordering::Relaxed);
                    if connection.take().is_some() {
                        eprintln!("Redis PUBLISH failed/timed out; delivery is unconfirmed. Forwarding disabled; stdout continues.");
                    }
                }
            }
        });
        Ok(Self {
            sender: Some(sender),
            worker,
            stats,
            enqueued: 0,
            dropped: 0,
        })
    }

    fn enqueue(&mut self, line: String) {
        if self
            .sender
            .as_ref()
            .expect("publisher is active")
            .try_send(line)
            .is_ok()
        {
            self.enqueued += 1;
        } else {
            self.dropped += 1;
            if self.dropped == 1 || self.dropped % 100 == 0 {
                eprintln!(
                    "Redis queue full/closed: {} messages skipped; JSONL stdout retains them.",
                    self.dropped
                );
            }
        }
    }

    async fn finish(mut self) -> Result<()> {
        self.sender.take();
        let mut worker_failed = false;
        match timeout(Duration::from_secs(5), &mut self.worker).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                worker_failed = true;
                eprintln!("Redis worker stopped unexpectedly: {error}");
            }
            Err(_) => {
                worker_failed = true;
                self.worker.abort();
                let _ = (&mut self.worker).await;
                eprintln!("Redis shutdown timeout; remaining delivery is unconfirmed.");
            }
        }
        let published = self.stats.published.load(Ordering::Relaxed);
        let failed = self.stats.failed.load(Ordering::Relaxed);
        let unconfirmed = self.enqueued.saturating_sub(published + failed);
        eprintln!("Redis: published={published}, failed_or_unconfirmed={failed}, queued_unconfirmed={unconfirmed}, queue_dropped={}", self.dropped);
        if worker_failed
            || self.stats.unavailable.load(Ordering::Relaxed)
            || failed + unconfirmed + self.dropped > 0
        {
            bail!(
                "Redis forwarding was unavailable or incomplete; use the JSONL capture to recover"
            );
        }
        Ok(())
    }
}
