use crate::{config::Config, storage::hex};
use anyhow::{anyhow, bail, Context, Result};
use bluer::{
    gatt::remote::Characteristic, Adapter, AdapterEvent, Address, Device, DeviceEvent,
    DeviceProperty, DiscoveryFilter, DiscoveryTransport, Session,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::{
    future::Future,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::timeout,
};
#[derive(Clone)]
pub struct Events {
    pub tx: mpsc::Sender<Value>,
    pub epoch: u64,
    pub start: Instant,
}
impl Events {
    pub async fn send(&self, mut v: Value) -> Result<()> {
        v["epoch"] = json!(self.epoch);
        v["observed_elapsed_ms"] = json!(self.start.elapsed().as_millis());
        self.tx
            .send(v)
            .await
            .map_err(|_| anyhow!("recorder stopped"))
    }
    pub async fn state(&self, s: &str) -> Result<()> {
        self.send(json!({"type":"connection","state":s})).await
    }
}
pub async fn bounded<T>(
    c: &Config,
    label: &str,
    f: impl Future<Output = bluer::Result<T>>,
) -> Result<T> {
    timeout(Duration::from_secs(c.timeout_seconds), f)
        .await
        .with_context(|| format!("{label}: timeout"))?
        .map_err(|e| anyhow!("{label}: {:?}", e.kind))
}
pub async fn adapter(c: &Config) -> Result<(Session, Adapter)> {
    let s = bounded(c, "open BlueZ", Session::new()).await?;
    let a = if let Some(name) = &c.adapter {
        s.adapter(name)?
    } else {
        bounded(c, "find adapter", s.default_adapter()).await?
    };
    if !bounded(c, "check adapter power", a.is_powered()).await? {
        bail!("selected Bluetooth adapter is powered off; use bluetoothctl to power it on");
    }
    Ok((s, a))
}
async fn discover(c: &Config, a: &Adapter) -> Result<impl futures::Stream<Item = AdapterEvent>> {
    bounded(
        c,
        "set LE discovery filter",
        a.set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            ..Default::default()
        }),
    )
    .await?;
    bounded(c, "start LE discovery", a.discover_devices_with_changes()).await
}
pub async fn scan(c: &Config, seconds: u64) -> Result<()> {
    let (_s, a) = adapter(c).await?;
    let stream = discover(c, &a).await?;
    futures::pin_mut!(stream);
    let end = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(end);
    loop {
        tokio::select! {_=&mut end=>break,event=stream.next()=>match event{
            Some(AdapterEvent::DeviceAdded(addr))=>{let d=a.device(addr)?;let row=json!({"type":"discovery_properties","address":addr.to_string(),"adapter":a.name(),"name":d.name().await.ok().flatten(),"rssi":d.rssi().await.ok().flatten(),"manufacturer_data":d.manufacturer_data().await.ok().flatten(),"service_data":d.service_data().await.ok().flatten(),"service_uuids":d.uuids().await.ok().flatten()});println!("{row}");},Some(_)=>{},None=>bail!("discovery stream ended")
        }}
    }
    Ok(())
}
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Capture,
    Inspect,
    Pair,
    Connect,
}
pub async fn run(
    c: Config,
    address: Address,
    mode: Mode,
    mut stop: watch::Receiver<bool>,
    mut out: Events,
) -> Result<()> {
    let mut backoff = 1u64;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        out.epoch += 1;
        out.state("connecting").await?;
        let result=async {
            let (s,a)=adapter(&c).await?;
            let d=a.device(address)?;
            let existing=d.is_connected().await.unwrap_or(false);
            let operation=async{
                // Keep discovery alive until the exact selected address has connected.
                let stream=discover(&c,&a).await?;futures::pin_mut!(stream);
                if !existing {
                    let wait=async{loop{if let Some(AdapterEvent::DeviceAdded(addr))=stream.next().await{if addr==address{break;}}}};
                    timeout(Duration::from_secs(c.timeout_seconds),wait).await.context("selected bike was not seen; wake it and disconnect the phone")?;
                }
                out.send(json!({"type":"device_properties","address":address.to_string(),"adapter":a.name(),"name":d.name().await.ok().flatten(),"manufacturer_data":d.manufacturer_data().await.ok().flatten(),"service_data":d.service_data().await.ok().flatten()})).await?;
                let _agent=if c.pair||mode==Mode::Pair{Some(bounded(&c,"register pairing agent",s.register_agent(bluer::agent::Agent::default())).await?)}else{None};
                if (c.pair||mode==Mode::Pair)&&!bounded(&c,"check bond",d.is_paired()).await?{
                    out.state("pairing").await?;
                    bounded(&c,"pair bike (PIN/passkey devices require bluetoothctl)",d.pair()).await?;
                }
                if !d.is_connected().await.unwrap_or(false){bounded(&c,"connect bike",d.connect()).await?;}
                out.state("connected").await?;
                if mode==Mode::Pair{return Ok(());}
                epoch(&c,&d,mode,&out).await
            };
            let result=tokio::select!{r=operation=>r,_=stop.changed()=>Ok(())};
            if !existing{
                let _=timeout(Duration::from_secs(3),d.disconnect()).await;
            }
            result
        }.await;
        if *stop.borrow() {
            out.state("stopped").await?;
            return Ok(());
        }
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                out.send(json!({"type":"coverage_gap","reason":format!("{e:#}"),"upstream_loss":"unknown"})).await?;
                out.state("disconnected").await?;
                if mode != Mode::Capture || !c.reconnect {
                    return Err(e);
                }
                out.send(json!({"type":"retry","delay_seconds":backoff}))
                    .await?;
                tokio::select! {_=tokio::time::sleep(Duration::from_secs(backoff))=>{},_=stop.changed()=>return Ok(())}
                backoff = (backoff * 2).min(c.reconnect_max_seconds);
            }
        }
    }
}
async fn value(out: &Events, source: &Value, kind: &str, bytes: Vec<u8>) -> Result<()> {
    let mut row = source.clone();
    row["type"] = json!(kind);
    row["value_hex"] = json!(hex(&bytes));
    row["value_length"] = json!(bytes.len());
    out.send(row).await
}
async fn read(
    c: &Config,
    out: &Events,
    source: &Value,
    ch: &Characteristic,
) -> Result<Option<Vec<u8>>> {
    let mut source = source.clone();
    source["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    let mut row = source.clone();
    row["type"] = json!("read_request");
    out.send(row).await?;
    match bounded(c, "read characteristic", ch.read()).await {
        Ok(bytes) => {
            value(out, &source, "read_result", bytes.clone()).await?;
            return Ok(Some(bytes));
        }
        Err(e) => {
            let mut row = source.clone();
            row["type"] = json!("read_error");
            row["error"] = json!(e.to_string());
            out.send(row).await?;
        }
    }
    Ok(None)
}
async fn epoch(c: &Config, d: &Device, mode: Mode, out: &Events) -> Result<()> {
    let events = bounded(c, "watch connection", d.events()).await?;
    futures::pin_mut!(events);
    let mut tasks = JoinSet::new();
    let mut reads = Vec::new();
    let mut subscriptions = 0;
    let mut writes = Vec::new();
    let (control_tx, mut control_rx) = tokio::sync::broadcast::channel::<(String, Vec<u8>)>(128);
    for service in bounded(c, "resolve GATT services", d.services()).await? {
        let su = bounded(c, "service UUID", service.uuid())
            .await?
            .to_string();
        out.send(json!({"type":"service","service_id":service.id(),"service_uuid":su}))
            .await?;
        let chars = match bounded(c, "enumerate characteristics", service.characteristics()).await {
            Ok(v) => v,
            Err(e) => {
                out.send(json!({"type":"inventory_error","service_id":service.id(),"error":e.to_string()})).await?;
                continue;
            }
        };
        for ch in chars {
            let cu = bounded(c, "characteristic UUID", ch.uuid())
                .await?
                .to_string();
            let flags = bounded(c, "characteristic flags", ch.flags()).await?;
            if flags.write_without_response || flags.write {
                writes.push((cu.clone(), ch.clone(), flags.write_without_response));
            }
            let source = json!({"service_id":service.id(),"service_uuid":su,"characteristic_id":ch.id(),"characteristic_uuid":cu});
            let mut row = source.clone();
            row["type"] = json!("characteristic");
            row["properties"] = json!(flags);
            out.send(row).await?;
            if mode == Mode::Capture && cu == "00002a00-0000-1000-8000-00805f9b34fb" {
                let mut row = source.clone();
                row["type"] = json!("subscription");
                row["outcome"] = json!("not_supported");
                row["reason"] =
                    json!("GAP Device Name has no CCCD on observed model; read instead");
                out.send(row).await?;
            }
            if mode == Mode::Capture
                && (flags.notify || flags.indicate)
                && cu != "00002a00-0000-1000-8000-00805f9b34fb"
            {
                let mut row = source.clone();
                row["type"] = json!("subscription");
                let io = if flags.notify {
                    bounded(c, "acquire notification socket", ch.notify_io())
                        .await
                        .ok()
                } else {
                    None
                };
                if let Some(io) = io {
                    row["outcome"] = json!("ok");
                    row["transport"] = json!("AcquireNotify");
                    out.send(row).await?;
                    subscriptions += 1;
                    let o = out.clone();
                    let src = source.clone();
                    let control = control_tx.clone();
                    tasks.spawn(async move {
                        loop {
                            let bytes = io.recv().await?;
                            if bytes.is_empty() {
                                bail!("notification socket ended");
                            }
                            let _ = control.send((
                                src["characteristic_uuid"].as_str().unwrap_or("").to_owned(),
                                bytes.clone(),
                            ));
                            value(&o, &src, "notification", bytes).await?;
                        }
                        #[allow(unreachable_code)]
                        Ok::<(), anyhow::Error>(())
                    });
                } else {
                    match bounded(c, "subscribe value changes", ch.notify()).await {
                        Ok(stream) => {
                            row["outcome"] = json!("ok");
                            row["transport"] = json!("StartNotify");
                            row["startup_gap_possible"] = json!(true);
                            out.send(row).await?;
                            subscriptions += 1;
                            let o = out.clone();
                            let src = source.clone();
                            let control = control_tx.clone();
                            tasks.spawn(async move {
                                futures::pin_mut!(stream);
                                while let Some(bytes) = stream.next().await {
                                    let _ = control.send((
                                        src["characteristic_uuid"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_owned(),
                                        bytes.clone(),
                                    ));
                                    value(&o, &src, "gatt_value_update", bytes).await?;
                                }
                                bail!("value stream ended")
                            });
                        }
                        Err(e) => {
                            row["outcome"] = json!("failed");
                            row["error"] = json!(e.to_string());
                            out.send(row).await?;
                        }
                    }
                }
            }
            if flags.read {
                reads.push((source.clone(), ch.clone()));
            }
            match bounded(c, "enumerate descriptors", ch.descriptors()).await {
                Ok(descs) => {
                    for desc in descs {
                        let mut row = source.clone();
                        row["type"] = json!("descriptor");
                        row["descriptor_id"] = json!(desc.id());
                        match bounded(c, "descriptor UUID", desc.uuid()).await {
                            Ok(u) => row["descriptor_uuid"] = json!(u.to_string()),
                            Err(e) => row["error"] = json!(e.to_string()),
                        }
                        out.send(row.clone()).await?;
                        row["type"] = json!("descriptor_read_request");
                        out.send(row.clone()).await?;
                        match bounded(c, "read descriptor", desc.read()).await {
                            Ok(b) => {
                                row["type"] = json!("descriptor_read_result");
                                row["value_hex"] = json!(hex(&b));
                                row["value_length"] = json!(b.len());
                            }
                            Err(e) => {
                                row["type"] = json!("descriptor_read_error");
                                row["error"] = json!(e.to_string());
                            }
                        }
                        out.send(row).await?;
                    }
                }
                Err(e) => {
                    let mut row = source.clone();
                    row["type"] = json!("inventory_error");
                    row["error"] = json!(e.to_string());
                    out.send(row).await?;
                }
            }
        }
    }
    let mut exact_model = false;
    for (src, ch) in &reads {
        if let Some(bytes) = read(c, out, src, ch).await? {
            if src["characteristic_uuid"] == "00002a24-0000-1000-8000-00805f9b34fb" {
                exact_model = bytes.split(|b| *b == 0).next() == Some(b"SF-B023005DGY".as_slice());
            }
        }
    }
    if mode == Mode::Inspect || mode == Mode::Connect {
        return Ok(());
    }
    let automatic = c.sunny_protocol && exact_model && c.init_command.is_empty();
    let commands = if automatic {
        crate::sunny::startup()
    } else {
        c.init_command
            .iter()
            .map(|s| {
                let (u, h) = s.split_once('=').context("invalid init command")?;
                Ok((u.to_owned(), crate::storage::unhex(h)?))
            })
            .collect::<Result<Vec<_>>>()?
    };
    // Discard earlier status traffic from the control path; raw recording already retained it.
    while control_rx.try_recv().is_ok() {}
    for (index, (uuid, bytes)) in commands.iter().enumerate() {
        write_command(c, out, &writes, uuid, bytes, "startup").await?;
        if automatic && index < 3 {
            let expected = if index == 1 { 1 } else { 2 };
            let response = async {
                let mut frame = crate::sunny::Framer::default();
                loop {
                    let (uuid, bytes) = control_rx
                        .recv()
                        .await
                        .context("startup response queue unavailable")?;
                    if uuid != crate::sunny::RESPONSE {
                        continue;
                    }
                    if let Some(sample) =
                        frame.push(&bytes, out.start.elapsed().as_millis() as u64, 0)?
                    {
                        if sample.extra["sunny_frames"][0]["command"] == expected
                            && sample.extra["sunny_frames"][0]["answer_flag"] == 0
                        {
                            return Ok::<(), anyhow::Error>(());
                        }
                        bail!("unexpected Sunny startup response");
                    }
                }
            };
            timeout(Duration::from_secs(c.timeout_seconds), response)
                .await
                .context("Sunny startup acknowledgement timed out")??;
            out.send(json!({"type":"startup_ack","step":index,"outcome":"ok"}))
                .await?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    out.send(json!({"type":"ready","subscriptions":subscriptions,"application_writes":commands.len(),"sunny_profile":automatic})).await?;
    out.state("ready").await?;
    let mut poll = tokio::time::interval(Duration::from_secs(c.poll_seconds.max(1)));
    poll.tick().await;
    loop {
        tokio::select! {
            message=control_rx.recv(), if automatic=>{if let Ok((uuid,bytes))=message {if uuid==crate::sunny::STATUS {
                let mut frame=crate::sunny::Framer::default();
                if frame.push(&bytes,out.start.elapsed().as_millis() as u64,0)?.is_some(){write_command(c,out,&writes,crate::sunny::STATUS_ACK,&crate::sunny::frame(4,&[]),"status_ack").await?;}
            }}},
            event=events.next()=>match event{Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)))|None=>bail!("bike disconnected"),Some(e)=>{out.send(json!({"type":"device_event","event":format!("{e:?}")})).await?;}},
            r=tasks.join_next(),if !tasks.is_empty()=>{r.context("notification task disappeared")???;bail!("notification task stopped");},
            _=poll.tick(),if c.poll_seconds>0=>{for (src,ch) in &reads{let _=read(c,out,src,ch).await?;}}
        }
    }
}
async fn write_command(
    c: &Config,
    out: &Events,
    writes: &[(String, Characteristic, bool)],
    uuid: &str,
    bytes: &[u8],
    reason: &str,
) -> Result<()> {
    let candidates: Vec<_> = writes
        .iter()
        .filter(|(u, _, _)| u.eq_ignore_ascii_case(uuid))
        .collect();
    if candidates.len() != 1 {
        bail!("initialization UUID must match exactly one writable characteristic");
    }
    let (_, ch, without_response) = candidates[0];
    out.send(json!({"type":"write_request","characteristic_uuid":uuid,"characteristic_id":ch.id(),"value_hex":hex(bytes),"value_length":bytes.len(),"reason":reason,"write_type":if *without_response {"command"}else{"request"}})).await?;
    let req = bluer::gatt::remote::CharacteristicWriteRequest {
        op_type: if *without_response {
            bluer::gatt::WriteOp::Command
        } else {
            bluer::gatt::WriteOp::Request
        },
        ..Default::default()
    };
    match bounded(c, "initialization write", ch.write_ext(bytes, &req)).await {
        Ok(()) => {
            out.send(json!({"type":"write_result","characteristic_uuid":uuid,"outcome":"ok"}))
                .await?
        }
        Err(e) => {
            out.send(
                json!({"type":"write_error","characteristic_uuid":uuid,"error":e.to_string()}),
            )
            .await?;
            return Err(e);
        }
    }
    Ok(())
}
pub async fn manage(c: &Config, address: Address, forget: bool) -> Result<()> {
    let (_s, a) = adapter(c).await?;
    if forget {
        if bounded(c, "list known devices", a.device_addresses())
            .await?
            .contains(&address)
        {
            bounded(c, "forget bike", a.remove_device(address)).await?;
        }
    } else {
        bounded(c, "disconnect bike", a.device(address)?.disconnect()).await?;
    }
    Ok(())
}
pub async fn doctor(c: &Config) -> Result<()> {
    let firmware = std::path::Path::new("/usr/lib/firmware/rtl_bt/rtl8761bu_fw.bin").exists()
        || std::path::Path::new("/lib/firmware/rtl_bt/rtl8761bu_fw.bin").exists();
    let s = bounded(c, "open BlueZ", Session::new()).await?;
    println!(
        "Realtek RTL8761BU firmware: {}",
        if firmware {
            "installed"
        } else {
            "missing; install firmware-realtek"
        }
    );
    for name in bounded(c, "list adapters", s.adapter_names()).await? {
        let a = s.adapter(&name)?;
        println!("{name}: powered={}", a.is_powered().await.unwrap_or(false));
    }
    Ok(())
}
