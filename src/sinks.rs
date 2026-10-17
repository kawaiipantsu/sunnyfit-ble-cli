//! Independent bounded database workers: slow storage never blocks BLE reception.
use crate::{config::Config, service};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::{any::AnyPoolOptions, AnyPool, Row};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};

#[derive(Default)]
struct Counters {
    written: AtomicU64,
    retries: AtomicU64,
}
pub struct Sink {
    name: String,
    tx: mpsc::Sender<Value>,
    task: JoinHandle<()>,
    counts: Arc<Counters>,
    queued: u64,
    dropped: u64,
}
fn endpoint(c: &Config, name: &str) -> Result<String> {
    match name {
        "mysql" => c
            .mysql_url
            .clone()
            .ok_or_else(|| anyhow!("mysql URL required")),
        "sqlite" => {
            if let Some(p) = c.sqlite_path.parent() {
                crate::config::private_dir(p)?;
            }
            let path = c
                .sqlite_path
                .to_str()
                .ok_or_else(|| anyhow!("SQLite path must be UTF-8"))?;
            // Use a URL serializer so spaces, #, ? and % in paths retain their meaning.
            let path = if c.sqlite_path.is_absolute() {
                c.sqlite_path.clone()
            } else {
                std::env::current_dir()?.join(path)
            };
            let mut url =
                reqwest::Url::from_file_path(path).map_err(|_| anyhow!("invalid SQLite path"))?;
            url.set_query(Some("mode=rwc"));
            Ok(url.as_str().replacen("file:", "sqlite:", 1))
        }
        _ => bail!("database must be sqlite or mysql"),
    }
}
async fn connect(url: &str) -> Result<AnyPool> {
    sqlx::any::install_default_drivers();
    AnyPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(4))
        .connect(url)
        .await
        .map_err(|_| anyhow!("database connection unavailable (details redacted)"))
}
async fn schema(pool: &AnyPool, mysql: bool) -> Result<()> {
    if !mysql {
        sqlx::query("PRAGMA journal_mode=WAL").execute(pool).await?;
        sqlx::query("PRAGMA busy_timeout=3000")
            .execute(pool)
            .await?;
    }
    let text = if mysql { "MEDIUMTEXT" } else { "TEXT" };
    for q in [
        format!("CREATE TABLE IF NOT EXISTS `workout-devices` (device_id VARCHAR(128) PRIMARY KEY, role VARCHAR(32) NOT NULL, profile VARCHAR(128) NOT NULL, last_seen_ms BIGINT NOT NULL, metadata_json {text} NOT NULL, schema_version INTEGER NOT NULL)"),
        format!("CREATE TABLE IF NOT EXISTS `workout-sessions` (session_id VARCHAR(64) PRIMARY KEY, started_ms BIGINT NOT NULL, ended_ms BIGINT NULL, device_id VARCHAR(128) NOT NULL, mode VARCHAR(32) NOT NULL, demo INTEGER NOT NULL, status VARCHAR(64) NOT NULL, elapsed_s DOUBLE NULL, active_s DOUBLE NULL, distance_m DOUBLE NULL, calories_kcal DOUBLE NULL, summary_json {text} NOT NULL, schema_version INTEGER NOT NULL)"),
        format!("CREATE TABLE IF NOT EXISTS `workout-statistics` (session_id VARCHAR(64) NOT NULL, sequence BIGINT NOT NULL, timestamp_ms BIGINT NOT NULL, elapsed_ms BIGINT NOT NULL, event_type VARCHAR(64) NOT NULL, device_role VARCHAR(32) NOT NULL, speed_kmh DOUBLE NULL, cadence_rpm DOUBLE NULL, distance_m DOUBLE NULL, calories_kcal DOUBLE NULL, power_w DOUBLE NULL, heart_rate_bpm DOUBLE NULL, payload_json {text} NOT NULL, PRIMARY KEY(session_id,sequence))"),
    ] {sqlx::query(&q).execute(pool).await?;}
    Ok(())
}
fn update(mysql: bool, key: &str, columns: &[&str]) -> String {
    if mysql {
        format!(
            " ON DUPLICATE KEY UPDATE {}",
            columns
                .iter()
                .map(|s| format!("{s}=VALUES({s})"))
                .collect::<Vec<_>>()
                .join(",")
        )
    } else {
        format!(
            " ON CONFLICT({key}) DO UPDATE SET {}",
            columns
                .iter()
                .map(|s| format!("{s}=excluded.{s}"))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}
async fn write(pool: &AnyPool, mysql: bool, c: &Config, row: &Value) -> Result<()> {
    let id = row["run_id"]
        .as_str()
        .ok_or_else(|| anyhow!("event missing session id"))?;
    let role = row["device_role"].as_str().unwrap_or("bike");
    let device = if role == "heart_rate" {
        c.heart_rate_device.as_deref()
    } else {
        c.device.as_deref()
    }
    .unwrap_or("unspecified");
    let device = format!("{role}:{device}");
    let bike = format!("bike:{}", c.device.as_deref().unwrap_or("unspecified"));
    let timestamp = row["timestamp_unix_ms"].as_i64().unwrap_or(0);
    let elapsed = row["elapsed_ms"].as_i64().unwrap_or(0);
    let kind = row["type"].as_str().unwrap_or("unknown");
    let w = &row["workout"];
    let summary = kind == "workout_summary";
    let q=format!("INSERT INTO `workout-sessions` (session_id,started_ms,ended_ms,device_id,mode,demo,status,elapsed_s,active_s,distance_m,calories_kcal,summary_json,schema_version) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,1){}",update(mysql,"session_id", if summary {&["ended_ms","status","elapsed_s","active_s","distance_m","calories_kcal","summary_json"]} else if kind=="metrics" {&["elapsed_s","active_s","distance_m","calories_kcal","summary_json"]} else {&["session_id"]}));
    let mut tx = pool.begin().await?;
    sqlx::query(&q)
        .bind(id)
        .bind(timestamp - elapsed)
        .bind(if summary { Some(timestamp) } else { None })
        .bind(&bike)
        .bind(if c.systemd { "systemd" } else { "cli" })
        .bind(i32::from(row["demo"].as_bool().unwrap_or(false)))
        .bind(if summary { "finished" } else { "recording" })
        .bind(w["elapsed_s"].as_f64())
        .bind(w["active_s"].as_f64())
        .bind(w["distance_m"].as_f64())
        .bind(w["calories_kcal"].as_f64())
        .bind(w.to_string())
        .execute(&mut *tx)
        .await?;
    let cols = if kind == "device_properties" {
        vec!["last_seen_ms", "profile", "metadata_json"]
    } else {
        vec!["last_seen_ms", "profile"]
    };
    let q=format!("INSERT INTO `workout-devices` (device_id,role,profile,last_seen_ms,metadata_json,schema_version) VALUES (?,?,?,?,?,1){}",update(mysql,"device_id",&cols));
    sqlx::query(&q)
        .bind(&device)
        .bind(role)
        .bind(if role == "heart_rate" {
            "heart-rate"
        } else {
            &c.profile
        })
        .bind(timestamp)
        .bind(if kind == "device_properties" {
            row.to_string()
        } else {
            "{}".into()
        })
        .execute(&mut *tx)
        .await?;
    let s = if kind == "decoded" {
        &row["sample"]
    } else {
        &w["current"]
    };
    let distance = if kind == "decoded" {
        s["distance_m"].as_f64()
    } else {
        w["distance_m"].as_f64()
    };
    let energy = if kind == "decoded" {
        s["calories_kcal"].as_f64()
    } else {
        w["calories_kcal"].as_f64()
    };
    let q=format!("INSERT INTO `workout-statistics` (session_id,sequence,timestamp_ms,elapsed_ms,event_type,device_role,speed_kmh,cadence_rpm,distance_m,calories_kcal,power_w,heart_rate_bpm,payload_json) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?){}",update(mysql,"session_id,sequence",&["payload_json"]));
    sqlx::query(&q)
        .bind(id)
        .bind(row["sequence"].as_i64().unwrap_or(0))
        .bind(timestamp)
        .bind(elapsed)
        .bind(kind)
        .bind(role)
        .bind(s["speed_kmh"].as_f64())
        .bind(s["cadence_rpm"].as_f64())
        .bind(distance)
        .bind(energy)
        .bind(s["power_w"].as_f64())
        .bind(s["heart_rate_bpm"].as_f64())
        .bind(row.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
impl Sink {
    pub fn start(c: &Config) -> Result<Vec<Self>> {
        let mut sinks = Vec::new();
        for name in c
            .sinks
            .iter()
            .filter(|s| matches!(s.as_str(), "mysql" | "sqlite"))
        {
            let url = endpoint(c, name)?;
            let mysql = name == "mysql";
            let name = name.clone();
            let label = name.clone();
            let (tx, mut rx) = mpsc::channel::<Value>(c.database_queue);
            let counts = Arc::new(Counters::default());
            let stats = counts.clone();
            let c = c.clone();
            let task = tokio::spawn(async move {
                let mut pool = None;
                let mut delay = 1;
                while let Some(row) = rx.recv().await {
                    loop {
                        let operation = async {
                            if pool.is_none() {
                                let p = connect(&url).await?;
                                schema(&p, mysql).await?;
                                pool = Some(p);
                            }
                            write(pool.as_ref().unwrap(), mysql, &c, &row).await
                        };
                        if matches!(timeout(Duration::from_secs(8), operation).await, Ok(Ok(()))) {
                            stats.written.fetch_add(1, Ordering::Relaxed);
                            delay = 1;
                            break;
                        }
                        pool = None;
                        stats.retries.fetch_add(1, Ordering::Relaxed);
                        service::log("storage_retry", json!({"sink":label,"retry_seconds":delay}));
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(30);
                    }
                }
            });
            sinks.push(Self {
                name,
                tx,
                task,
                counts,
                queued: 0,
                dropped: 0,
            });
        }
        Ok(sinks)
    }
    pub fn enqueue(&mut self, row: &Value) {
        if self.tx.try_send(row.clone()).is_ok() {
            self.queued += 1;
        } else {
            self.dropped += 1;
        }
    }
    pub fn stats(&self) -> Value {
        let written = self.counts.written.load(Ordering::Relaxed);
        json!({"sink":self.name,"written":written,"pending":self.queued.saturating_sub(written),"queue_dropped":self.dropped,"retries":self.counts.retries.load(Ordering::Relaxed)})
    }
    pub async fn finish(mut self) -> Value {
        // Close the queue without partially moving self (stats remain accessible).
        let (dummy, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.tx, dummy));
        if timeout(Duration::from_secs(8), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        self.stats()
    }
}
fn text_column(row: &sqlx::any::AnyRow, name: &str) -> Result<String> {
    if let Ok(text) = row.try_get::<String, _>(name) {
        return Ok(text);
    }
    Ok(String::from_utf8(row.try_get::<Vec<u8>, _>(name)?)?)
}
pub async fn sessions(c: &Config, database: &str) -> Result<Vec<Value>> {
    let p = connect(&endpoint(c, database)?).await?;
    let rows=sqlx::query("SELECT session_id,started_ms,ended_ms,status,summary_json FROM `workout-sessions` ORDER BY started_ms DESC LIMIT 100").fetch_all(&p).await.map_err(|_|anyhow!("cannot read sessions; initialize the selected database first"))?;
    rows.iter().map(|r|Ok(json!({"session_id":r.try_get::<String,_>("session_id")?,"started_ms":r.try_get::<i64,_>("started_ms")?,"ended_ms":r.try_get::<Option<i64>,_>("ended_ms")?,"status":r.try_get::<String,_>("status")?,"workout":serde_json::from_str::<Value>(&text_column(r,"summary_json")?)?}))).collect()
}
pub async fn events(c: &Config, database: &str, session: &str) -> Result<Vec<Value>> {
    let p = connect(&endpoint(c, database)?).await?;
    let rows=sqlx::query("SELECT payload_json FROM `workout-statistics` WHERE session_id=? AND event_type IN ('metrics','workout_summary') ORDER BY sequence").bind(session).fetch_all(&p).await.map_err(|_|anyhow!("cannot read session statistics"))?;
    rows.iter()
        .map(|r| Ok(serde_json::from_str(&text_column(r, "payload_json")?)?))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn sqlite_roundtrip_idempotence_and_missing_fields() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config {
            sqlite_path: dir.join("workouts.sqlite"),
            ..Default::default()
        };
        let p = connect(&endpoint(&c, "sqlite").unwrap()).await.unwrap();
        schema(&p, false).await.unwrap();
        let row = json!({"run_id":"test","sequence":1,"timestamp_unix_ms":5000,"elapsed_ms":1000,"type":"metrics","workout":{"elapsed_s":1.,"active_s":1.,"distance_m":2.,"current":{"cadence_rpm":60.}}});
        write(&p, false, &c, &row).await.unwrap();
        write(&p, false, &c, &row).await.unwrap();
        assert_eq!(events(&c, "sqlite", "test").await.unwrap().len(), 1);
        let r = sqlx::query("SELECT heart_rate_bpm,cadence_rpm FROM `workout-statistics`")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(r.try_get::<Option<f64>, _>("heart_rate_bpm").unwrap(), None);
        assert_eq!(r.try_get::<f64, _>("cadence_rpm").unwrap(), 60.);
        p.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
