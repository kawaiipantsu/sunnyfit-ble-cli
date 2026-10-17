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
        format!("CREATE TABLE IF NOT EXISTS `workout-sessions` (session_id VARCHAR(64) PRIMARY KEY, started_ms BIGINT NOT NULL, ended_ms BIGINT NULL, device_id VARCHAR(128) NOT NULL, mode VARCHAR(32) NOT NULL, demo INTEGER NOT NULL, status VARCHAR(64) NOT NULL, elapsed_s DOUBLE NULL, active_s DOUBLE NULL, distance_m DOUBLE NULL, calories_kcal DOUBLE NULL, summary_json {text} NOT NULL, schema_version INTEGER NOT NULL, session_kind VARCHAR(16) NOT NULL DEFAULT 'collector', parent_session_id VARCHAR(64) NULL, last_sequence BIGINT NOT NULL DEFAULT 0)"),
        crate::database_stats::create_table(),
    ] { sqlx::query(&q).execute(pool).await?; }
    if sqlx::query("SELECT statistic_id FROM `workout-statistics` LIMIT 0")
        .execute(pool)
        .await
        .is_err()
    {
        bail!("legacy statistics table: stop acquisition and run reset-statistics --database mysql|sqlite --confirm-reset; this deletes only old statistics");
    }
    for column in [
        "cumulative_active_s",
        "cumulative_distance_m",
        "cumulative_calories_kcal",
    ] {
        if sqlx::query(&format!(
            "SELECT {column} FROM `workout-statistics` LIMIT 0"
        ))
        .execute(pool)
        .await
        .is_err()
        {
            sqlx::query(&format!(
                "ALTER TABLE `workout-statistics` ADD COLUMN {column} DOUBLE NULL"
            ))
            .execute(pool)
            .await?;
        }
    }
    // Additive migration preserves previous collector sessions and devices.
    for (column, definition) in [
        ("session_kind", "VARCHAR(16) NOT NULL DEFAULT 'collector'"),
        ("parent_session_id", "VARCHAR(64) NULL"),
        ("last_sequence", "BIGINT NOT NULL DEFAULT 0"),
    ] {
        if sqlx::query(&format!("SELECT {column} FROM `workout-sessions` LIMIT 0"))
            .execute(pool)
            .await
            .is_err()
        {
            sqlx::query(&format!(
                "ALTER TABLE `workout-sessions` ADD COLUMN {column} {definition}"
            ))
            .execute(pool)
            .await?;
        }
    }
    for (index, columns) in [
        ("statistics_chart", "scope,demo,bucket_start_ms,device_id"),
        ("statistics_session", "session_id,scope,bucket_start_ms"),
        ("statistics_device", "device_id,scope,demo,bucket_start_ms"),
    ] {
        if mysql {
            let found: i64=sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.statistics WHERE table_schema=DATABASE() AND table_name='workout-statistics' AND index_name=?").bind(index).fetch_one(pool).await?;
            if found == 0 {
                sqlx::query(&format!(
                    "CREATE INDEX {index} ON `workout-statistics` ({columns})"
                ))
                .execute(pool)
                .await?;
            }
        } else {
            sqlx::query(&format!(
                "CREATE INDEX IF NOT EXISTS {index} ON `workout-statistics` ({columns})"
            ))
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
pub async fn reset_statistics(c: &Config, database: &str) -> Result<()> {
    let pool = connect(&endpoint(c, database)?).await?;
    sqlx::query("DROP TABLE IF EXISTS `workout-statistics`")
        .execute(&pool)
        .await
        .map_err(|_| anyhow!("cannot reset statistics (database details redacted)"))?;
    schema(&pool, database == "mysql")
        .await
        .map_err(|_| anyhow!("cannot initialize aggregate schema (database details redacted)"))?;
    Ok(())
}
pub(crate) fn update(mysql: bool, key: &str, columns: &[&str]) -> String {
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
    let address = if role == "heart_rate" {
        c.heart_rate_device.as_deref()
    } else {
        c.device.as_deref()
    }
    .unwrap_or("unspecified");
    let device = format!("{role}:{address}");
    let bike = format!("bike:{}", c.device.as_deref().unwrap_or("unspecified"));
    let timestamp = row["timestamp_unix_ms"].as_i64().unwrap_or(0);
    let elapsed = row["elapsed_ms"].as_i64().unwrap_or(0);
    let sequence = row["sequence"].as_i64().unwrap_or(0);
    let kind = row["type"].as_str().unwrap_or("unknown");
    let w = &row["workout"];
    let summary = kind == "workout_summary";
    let demo = i32::from(row["demo"].as_bool().unwrap_or(false));
    let mut tx = pool.begin().await?;
    let q=format!("INSERT INTO `workout-sessions` (session_id,started_ms,device_id,mode,demo,status,summary_json,schema_version) VALUES (?,?,?,?,?,'recording','{{}}',2){}",update(mysql,"session_id",&["session_id"]));
    sqlx::query(&q)
        .bind(id)
        .bind(timestamp - elapsed)
        .bind(&bike)
        .bind(if c.systemd { "systemd" } else { "cli" })
        .bind(demo)
        .execute(&mut *tx)
        .await?;
    let q = format!(
        "SELECT last_sequence FROM `workout-sessions` WHERE session_id=?{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let last: i64 = sqlx::query_scalar(&q).bind(id).fetch_one(&mut *tx).await?;
    if last >= sequence {
        tx.commit().await?;
        return Ok(());
    }
    let cols = if kind == "device_properties" {
        vec!["last_seen_ms", "profile", "metadata_json", "schema_version"]
    } else {
        vec!["last_seen_ms", "profile", "schema_version"]
    };
    let q=format!("INSERT INTO `workout-devices` (device_id,role,profile,last_seen_ms,metadata_json,schema_version) VALUES (?,?,?,?,?,2){}",update(mysql,"device_id",&cols));
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
    if matches!(kind, "metrics" | "workout_summary") {
        let observation: crate::statistics::Observation =
            serde_json::from_value(row["statistics"].clone())?;
        crate::database_stats::apply(&mut tx, mysql, &bike, row, &observation).await?;
        sqlx::query("UPDATE `workout-sessions` SET ended_ms=?,status=?,elapsed_s=?,active_s=?,distance_m=?,calories_kcal=?,summary_json=?,schema_version=2 WHERE session_id=?")
            .bind(if summary{Some(timestamp)}else{None}).bind(if summary{"finished"}else{"recording"})
            .bind(w["elapsed_s"].as_f64()).bind(w["active_s"].as_f64()).bind(w["distance_m"].as_f64()).bind(w["calories_kcal"].as_f64())
            .bind(w.to_string()).bind(id).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE `workout-sessions` SET last_sequence=? WHERE session_id=?")
        .bind(sequence)
        .bind(id)
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
        if !matches!(
            row["type"].as_str(),
            Some("metrics" | "workout_summary" | "device_properties")
        ) {
            return;
        }
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
    let rows=sqlx::query("SELECT session_id,started_ms,ended_ms,status,session_kind,parent_session_id,summary_json FROM `workout-sessions` ORDER BY started_ms DESC LIMIT 100").fetch_all(&p).await.map_err(|_|anyhow!("cannot read sessions; initialize the selected database first"))?;
    rows.iter().map(|r|Ok(json!({"session_id":r.try_get::<String,_>("session_id")?,"started_ms":r.try_get::<i64,_>("started_ms")?,"ended_ms":r.try_get::<Option<i64>,_>("ended_ms")?,"status":r.try_get::<String,_>("status")?,"session_kind":r.try_get::<String,_>("session_kind")?,"parent_session_id":r.try_get::<Option<String>,_>("parent_session_id")?,"workout":serde_json::from_str::<Value>(&text_column(r,"summary_json")?)?}))).collect()
}
pub async fn events(c: &Config, database: &str, session: &str) -> Result<Vec<Value>> {
    let p = connect(&endpoint(c, database)?).await?;
    let s = sqlx::query(
        "SELECT started_ms,summary_json,session_kind FROM `workout-sessions` WHERE session_id=?",
    )
    .bind(session)
    .fetch_one(&p)
    .await
    .map_err(|_| anyhow!("cannot read session"))?;
    let started: i64 = s.try_get("started_ms")?;
    let kind: String = s.try_get("session_kind")?;
    let summary: Value = serde_json::from_str(&text_column(&s, "summary_json")?)?;
    let end = started + (summary["elapsed_s"].as_f64().unwrap_or(0.) * 1000.).round() as i64;
    let scope = if kind == "trip" {
        "trip-minute"
    } else {
        "session-minute"
    };
    let rows=sqlx::query("SELECT updated_ms,active_s,distance_m,calories_kcal,speed_average_kmh,cadence_average_rpm,heart_rate_average_bpm,power_average_w FROM `workout-statistics` WHERE session_id=? AND scope=? AND bucket_start_ms<=? ORDER BY bucket_start_ms").bind(session).bind(scope).bind(end).fetch_all(&p).await.map_err(|_|anyhow!("cannot read aggregate statistics"))?;
    let mut result = Vec::new();
    let mut active = 0.;
    let mut distance = None;
    let mut calories = None;
    for r in rows {
        let timestamp: i64 = r.try_get::<i64, _>("updated_ms")?.min(end);
        active += r.try_get::<f64, _>("active_s")?;
        for (total, col) in [
            (&mut distance, "distance_m"),
            (&mut calories, "calories_kcal"),
        ] {
            if let Some(value) = r.try_get::<Option<f64>, _>(col)? {
                *total.get_or_insert(0.) += value;
            }
        }
        result.push(json!({"type":"metrics","run_id":session,"timestamp_unix_ms":timestamp,"elapsed_ms":timestamp-started,"workout":{"elapsed_s":(timestamp-started) as f64/1000.,"active_s":active,"distance_m":distance,"calories_kcal":calories,"current":{"speed_kmh":r.try_get::<Option<f64>,_>("speed_average_kmh")?,"cadence_rpm":r.try_get::<Option<f64>,_>("cadence_average_rpm")?,"heart_rate_bpm":r.try_get::<Option<f64>,_>("heart_rate_average_bpm")?,"power_w":r.try_get::<Option<f64>,_>("power_average_w")?}}}));
    }
    if let Some(last) = result.last() {
        let workout = summary;
        result.push(json!({"type":"workout_summary","run_id":session,"timestamp_unix_ms":last["timestamp_unix_ms"],"elapsed_ms":last["elapsed_ms"],"workout":workout}));
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn aggregates_are_materialized_idempotent_and_preserve_unknowns() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config {
            sqlite_path: dir.join("workouts.sqlite"),
            ..Default::default()
        };
        let p = connect(&endpoint(&c, "sqlite").unwrap()).await.unwrap();
        schema(&p, false).await.unwrap();
        let mut tracker = crate::statistics::Tracker::default();
        let mut w = crate::metrics::Workout::default();
        w.connection("ready");
        w.stale = false;
        w.current.speed_kmh = Some(18.);
        w.current.cadence_rpm = Some(60.);
        let start = 1_700_000_000_000_i64;
        for (sequence, elapsed, speed, dist) in [(1, 1000, 18., 5.), (2, 2000, 36., 15.)] {
            w.elapsed_s = elapsed as f64 / 1000.;
            w.active_s = w.elapsed_s;
            w.distance_m = Some(dist);
            w.current.speed_kmh = Some(speed);
            let obs = tracker.observe(&w, &c, elapsed, start + elapsed as i64, false);
            let row = json!({"run_id":"test","sequence":sequence,"timestamp_unix_ms":start+elapsed as i64,"elapsed_ms":elapsed,"type":"metrics","workout":w,"statistics":obs});
            write(&p, false, &c, &row).await.unwrap();
            write(&p, false, &c, &row).await.unwrap();
        }
        let row=sqlx::query("SELECT distance_m,active_s,samples,speed_average_kmh,speed_min_kmh,speed_max_kmh,heart_rate_average_bpm,calories_kcal,trips_started,sessions_started FROM `workout-statistics` WHERE scope='lifetime'").fetch_one(&p).await.unwrap();
        assert_eq!(row.try_get::<f64, _>("distance_m").unwrap(), 15.);
        assert_eq!(row.try_get::<f64, _>("active_s").unwrap(), 2.);
        assert_eq!(row.try_get::<i64, _>("samples").unwrap(), 2);
        assert_eq!(row.try_get::<f64, _>("speed_average_kmh").unwrap(), 27.);
        assert_eq!(row.try_get::<f64, _>("speed_min_kmh").unwrap(), 18.);
        assert_eq!(row.try_get::<f64, _>("speed_max_kmh").unwrap(), 36.);
        assert_eq!(
            row.try_get::<Option<f64>, _>("heart_rate_average_bpm")
                .unwrap(),
            None
        );
        assert_eq!(
            row.try_get::<Option<f64>, _>("calories_kcal").unwrap(),
            None
        );
        assert_eq!(row.try_get::<i64, _>("trips_started").unwrap(), 1);
        assert_eq!(row.try_get::<i64, _>("sessions_started").unwrap(), 1);
        w.current.cadence_rpm = Some(0.);
        w.current.speed_kmh = Some(0.);
        w.elapsed_s = 302.;
        let obs = tracker.observe(&w, &c, 302000, start + 302000, false);
        let trip = obs.closed_trip.as_ref().unwrap().id.clone();
        let row = json!({"run_id":"test","sequence":3,"timestamp_unix_ms":start+302000,"elapsed_ms":302000,"type":"metrics","workout":w,"statistics":obs});
        write(&p, false, &c, &row).await.unwrap();
        write(&p, false, &c, &row).await.unwrap();
        let cumulative:f64=sqlx::query_scalar("SELECT cumulative_distance_m FROM `workout-statistics` WHERE scope='session-minute' ORDER BY bucket_start_ms DESC LIMIT 1").fetch_one(&p).await.unwrap();
        assert_eq!(cumulative, 15.);
        let completed: i64 = sqlx::query_scalar(
            "SELECT trips_completed FROM `workout-statistics` WHERE scope='lifetime'",
        )
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(completed, 1);
        let trip_stats = sqlx::query(
            "SELECT active_s,elapsed_s,distance_m FROM `workout-statistics` WHERE scope='trip'",
        )
        .fetch_one(&p)
        .await
        .unwrap();
        assert_eq!(trip_stats.try_get::<f64, _>("elapsed_s").unwrap(), 2.);
        assert_eq!(trip_stats.try_get::<f64, _>("distance_m").unwrap(), 15.);
        let trip_row =
            sqlx::query("SELECT status,ended_ms FROM `workout-sessions` WHERE session_id=?")
                .bind(&trip)
                .fetch_one(&p)
                .await
                .unwrap();
        assert_eq!(trip_row.try_get::<String, _>("status").unwrap(), "finished");
        assert_eq!(
            trip_row.try_get::<i64, _>("ended_ms").unwrap(),
            start + 2000
        );
        let exported = events(&c, "sqlite", &trip).await.unwrap();
        assert!(!exported.is_empty());
        assert!(exported
            .iter()
            .all(|v| v["timestamp_unix_ms"].as_i64().unwrap() <= start + 2000));
        p.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn explicit_reset_preserves_legacy_sessions_and_devices() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config {
            sqlite_path: dir.join("workouts.sqlite"),
            ..Default::default()
        };
        let p = connect(&endpoint(&c, "sqlite").unwrap()).await.unwrap();
        sqlx::query("CREATE TABLE `workout-sessions` (session_id VARCHAR(64) PRIMARY KEY,started_ms BIGINT NOT NULL,ended_ms BIGINT NULL,device_id VARCHAR(128) NOT NULL,mode VARCHAR(32) NOT NULL,demo INTEGER NOT NULL,status VARCHAR(64) NOT NULL,elapsed_s DOUBLE NULL,active_s DOUBLE NULL,distance_m DOUBLE NULL,calories_kcal DOUBLE NULL,summary_json TEXT NOT NULL,schema_version INTEGER NOT NULL)").execute(&p).await.unwrap();
        sqlx::query("CREATE TABLE `workout-statistics` (session_id TEXT, payload_json TEXT)")
            .execute(&p)
            .await
            .unwrap();
        assert!(schema(&p, false).await.is_err());
        sqlx::query("INSERT INTO `workout-devices` VALUES ('test','bike','test',0,'{}',1)")
            .execute(&p)
            .await
            .unwrap();
        sqlx::query("INSERT INTO `workout-sessions` (session_id,started_ms,device_id,mode,demo,status,summary_json,schema_version) VALUES ('old',0,'test','systemd',0,'finished','{}',1)").execute(&p).await.unwrap();
        reset_statistics(&c, "sqlite").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `workout-sessions`")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `workout-devices`")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `workout-statistics`")
            .fetch_one(&p)
            .await
            .unwrap();
        assert_eq!(count, 0);
        p.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
