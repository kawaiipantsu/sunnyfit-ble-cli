//! Materialized chart buckets. Every update shares the session's retry/dedup transaction.
use crate::statistics::{Observation, Totals, Trip};
use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};
use serde_json::Value;
use sqlx::{Any, Transaction};

const COUNTS: &[&str] = &[
    "samples",
    "sessions_started",
    "trips_started",
    "trips_completed",
];
const SUMS: &[&str] = &[
    "elapsed_s",
    "active_s",
    "trip_elapsed_s",
    "speed_integral",
    "speed_observed_s",
    "cadence_integral",
    "cadence_observed_s",
    "heart_rate_integral",
    "heart_rate_observed_s",
    "power_integral",
    "power_observed_s",
];
const OPTIONAL_SUMS: &[&str] = &["distance_m", "calories_kcal"];
const MINIMA: &[&str] = &[
    "speed_min_kmh",
    "cadence_min_rpm",
    "heart_rate_min_bpm",
    "power_min_w",
];
const MAXIMA: &[&str] = &[
    "speed_max_kmh",
    "cadence_max_rpm",
    "heart_rate_max_bpm",
    "power_max_w",
];
const AVERAGES: &[(&str, &str, &str)] = &[
    ("speed_average_kmh", "speed_integral", "speed_observed_s"),
    (
        "cadence_average_rpm",
        "cadence_integral",
        "cadence_observed_s",
    ),
    (
        "heart_rate_average_bpm",
        "heart_rate_integral",
        "heart_rate_observed_s",
    ),
    ("power_average_w", "power_integral", "power_observed_s"),
];
fn columns() -> Vec<&'static str> {
    COUNTS
        .iter()
        .chain(SUMS)
        .chain(OPTIONAL_SUMS)
        .chain(MINIMA)
        .chain(MAXIMA)
        .copied()
        .collect()
}
pub fn create_table() -> String {
    let mut fields = vec![
        "statistic_id VARCHAR(240) PRIMARY KEY".to_owned(),
        "scope VARCHAR(32) NOT NULL".into(),
        "device_id VARCHAR(128) NOT NULL".into(),
        "session_id VARCHAR(64) NULL".into(),
        "bucket_start_ms BIGINT NOT NULL".into(),
        "bucket_end_ms BIGINT NULL".into(),
        "demo INTEGER NOT NULL".into(),
        "updated_ms BIGINT NOT NULL".into(),
        "cumulative_active_s DOUBLE NULL".into(),
        "cumulative_distance_m DOUBLE NULL".into(),
        "cumulative_calories_kcal DOUBLE NULL".into(),
    ];
    for name in columns() {
        let ty = if COUNTS.contains(&name) {
            "BIGINT NOT NULL"
        } else if SUMS.contains(&name) {
            "DOUBLE NOT NULL"
        } else {
            "DOUBLE NULL"
        };
        fields.push(format!("{name} {ty}"));
    }
    for (name, _, _) in AVERAGES {
        fields.push(format!("{name} DOUBLE NULL"));
    }
    format!(
        "CREATE TABLE IF NOT EXISTS `workout-statistics` ({})",
        fields.join(",")
    )
}
struct Bucket {
    scope: &'static str,
    start: i64,
    end: Option<i64>,
    session: Option<String>,
    cumulative: Option<(f64, Option<f64>, Option<f64>)>,
}
fn calendar(timestamp: i64) -> Vec<Bucket> {
    let dt = Utc.timestamp_millis_opt(timestamp).single().unwrap();
    let month = Utc
        .with_ymd_and_hms(dt.year(), dt.month(), 1, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    let (year, next_month) = if dt.month() == 12 {
        (dt.year() + 1, 1)
    } else {
        (dt.year(), dt.month() + 1)
    };
    let month_end = Utc
        .with_ymd_and_hms(year, next_month, 1, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    let day = timestamp.div_euclid(86_400_000) * 86_400_000;
    let week = day - i64::from(dt.weekday().num_days_from_monday()) * 86_400_000;
    let mut result = Vec::new();
    for (scope, span) in [("minute", 60_000), ("hour", 3_600_000), ("day", 86_400_000)] {
        let start = timestamp.div_euclid(span) * span;
        result.push(Bucket {
            scope,
            start,
            end: Some(start + span),
            session: None,
            cumulative: None,
        });
    }
    result.push(Bucket {
        scope: "week",
        start: week,
        end: Some(week + 7 * 86_400_000),
        session: None,
        cumulative: None,
    });
    result.push(Bucket {
        scope: "month",
        start: month,
        end: Some(month_end),
        session: None,
        cumulative: None,
    });
    result.push(Bucket {
        scope: "lifetime",
        start: 0,
        end: None,
        session: None,
        cumulative: None,
    });
    result
}
fn session_buckets(
    id: &str,
    start: i64,
    end: Option<i64>,
    timestamp: i64,
    trip: bool,
    cumulative: Option<(f64, Option<f64>, Option<f64>)>,
) -> Vec<Bucket> {
    let minute = timestamp.div_euclid(60_000) * 60_000;
    vec![
        Bucket {
            scope: if trip { "trip" } else { "session" },
            start,
            end,
            session: Some(id.into()),
            cumulative,
        },
        Bucket {
            scope: if trip {
                "trip-minute"
            } else {
                "session-minute"
            },
            start: minute,
            end: Some(minute + 60_000),
            session: Some(id.into()),
            cumulative,
        },
    ]
}
async fn upsert(
    tx: &mut Transaction<'_, Any>,
    mysql: bool,
    device: &str,
    demo: bool,
    timestamp: i64,
    bucket: Bucket,
    totals: &Totals,
) -> Result<()> {
    let calendar_key = if matches!(bucket.scope, "session" | "trip") {
        0
    } else {
        bucket.start
    };
    let key = format!(
        "{}:{}:{}:{}:{}",
        bucket.scope,
        device,
        i32::from(demo),
        bucket.session.as_deref().unwrap_or(""),
        calendar_key
    );
    let values = serde_json::to_value(totals)?;
    let numeric = columns();
    let averages = AVERAGES.iter().map(|(n, _, _)| *n).collect::<Vec<_>>();
    let mut names = vec![
        "statistic_id",
        "scope",
        "device_id",
        "session_id",
        "bucket_start_ms",
        "bucket_end_ms",
        "demo",
        "updated_ms",
        "cumulative_active_s",
        "cumulative_distance_m",
        "cumulative_calories_kcal",
    ];
    names.extend(&numeric);
    names.extend(&averages);
    let incoming = |col: &str| {
        if mysql {
            format!("VALUES({col})")
        } else {
            format!("excluded.{col}")
        }
    };
    let mut updates = Vec::new();
    // MySQL assignments are evaluated left-to-right: averages precede their sums.
    for (name, sum, seconds) in AVERAGES {
        updates.push(format!(
            "{name}=({sum}+{})/NULLIF({seconds}+{},0)",
            incoming(sum),
            incoming(seconds)
        ));
    }
    for name in COUNTS.iter().chain(SUMS) {
        updates.push(format!("{name}={name}+{}", incoming(name)));
    }
    for name in OPTIONAL_SUMS {
        updates.push(format!("{name}=CASE WHEN {name} IS NULL AND {} IS NULL THEN NULL ELSE COALESCE({name},0)+COALESCE({},0) END",incoming(name),incoming(name)));
    }
    for (list, comparison) in [(MINIMA, "<"), (MAXIMA, ">")] {
        for name in list {
            let v = incoming(name);
            updates.push(format!(
                "{name}=CASE WHEN {name} IS NULL OR {v}{comparison}{name} THEN {v} ELSE {name} END"
            ));
        }
    }
    updates.push(format!(
        "updated_ms=CASE WHEN updated_ms>{0} THEN updated_ms ELSE {0} END",
        incoming("updated_ms")
    ));
    updates.push(format!(
        "bucket_end_ms=COALESCE({},bucket_end_ms)",
        incoming("bucket_end_ms")
    ));
    for name in [
        "cumulative_active_s",
        "cumulative_distance_m",
        "cumulative_calories_kcal",
    ] {
        updates.push(format!("{name}=COALESCE({},{name})", incoming(name)));
    }
    let conflict = if mysql {
        " ON DUPLICATE KEY UPDATE "
    } else {
        " ON CONFLICT(statistic_id) DO UPDATE SET "
    };
    let q = format!(
        "INSERT INTO `workout-statistics` ({}) VALUES ({}){conflict}{}",
        names.join(","),
        vec!["?"; names.len()].join(","),
        updates.join(",")
    );
    let mut query = sqlx::query(&q)
        .bind(key)
        .bind(bucket.scope)
        .bind(device)
        .bind(bucket.session)
        .bind(bucket.start)
        .bind(bucket.end)
        .bind(i32::from(demo))
        .bind(timestamp)
        .bind(bucket.cumulative.map(|v| v.0))
        .bind(bucket.cumulative.and_then(|v| v.1))
        .bind(bucket.cumulative.and_then(|v| v.2));
    for name in numeric {
        query = if COUNTS.contains(&name) {
            query.bind(values[name].as_i64().unwrap_or(0))
        } else {
            query.bind(values[name].as_f64())
        };
    }
    for (_, sum, seconds) in AVERAGES {
        let secs = values[seconds].as_f64().unwrap_or(0.);
        query = query.bind((secs > 0.).then(|| values[sum].as_f64().unwrap_or(0.) / secs));
    }
    query.execute(&mut **tx).await?;
    Ok(())
}
async fn save_trip(
    tx: &mut Transaction<'_, Any>,
    mysql: bool,
    device: &str,
    run: &str,
    demo: bool,
    trip: &Trip,
) -> Result<()> {
    let q=format!("INSERT INTO `workout-sessions` (session_id,started_ms,ended_ms,device_id,mode,demo,status,elapsed_s,active_s,distance_m,calories_kcal,summary_json,schema_version,session_kind,parent_session_id,last_sequence) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,2,'trip',?,0){}",super::sinks::update(mysql,"session_id",&["ended_ms","status","elapsed_s","active_s","distance_m","calories_kcal","summary_json"]));
    sqlx::query(&q)
        .bind(&trip.id)
        .bind(trip.started_ms)
        .bind(trip.ended_ms)
        .bind(device)
        .bind("automatic")
        .bind(i32::from(demo))
        .bind(if trip.ended_ms.is_some() {
            "finished"
        } else {
            "recording"
        })
        .bind(trip.elapsed_s)
        .bind(trip.active_s)
        .bind(trip.distance_m)
        .bind(trip.calories_kcal)
        .bind(serde_json::to_string(trip)?)
        .bind(run)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn apply(
    tx: &mut Transaction<'_, Any>,
    mysql: bool,
    device: &str,
    row: &Value,
    observation: &Observation,
) -> Result<()> {
    let timestamp = row["timestamp_unix_ms"].as_i64().unwrap_or(0);
    let run = row["run_id"].as_str().unwrap();
    let started = timestamp - row["elapsed_ms"].as_i64().unwrap_or(0);
    let ending = row["type"] == "workout_summary";
    let demo = row["demo"].as_bool().unwrap_or(false);
    let cumulative = Some((
        row["workout"]["active_s"].as_f64().unwrap_or(0.),
        row["workout"]["distance_m"].as_f64(),
        row["workout"]["calories_kcal"].as_f64(),
    ));
    let mut buckets = calendar(timestamp);
    buckets.extend(session_buckets(
        run,
        started,
        ending.then_some(timestamp),
        timestamp,
        false,
        cumulative,
    ));
    for bucket in buckets {
        upsert(
            tx,
            mysql,
            device,
            demo,
            timestamp,
            bucket,
            &observation.delta,
        )
        .await?;
    }
    if let Some(trip) = &observation.trip {
        save_trip(tx, mysql, device, run, demo, trip).await?;
        let mut delta = observation.delta.clone();
        delta.sessions_started = 0;
        delta.elapsed_s = delta.trip_elapsed_s;
        for bucket in session_buckets(
            &trip.id,
            trip.started_ms,
            trip.ended_ms,
            timestamp,
            true,
            Some((trip.active_s, trip.distance_m, trip.calories_kcal)),
        ) {
            upsert(tx, mysql, device, demo, timestamp, bucket, &delta).await?;
        }
    }
    if let Some(trip) = &observation.closed_trip {
        save_trip(tx, mysql, device, run, demo, trip).await?;
        let totals = Totals {
            trips_completed: 1,
            ..Default::default()
        };
        let mut buckets = calendar(trip.last_moving_ms);
        // Completion increments the parent summary, never creates a second timeline point.
        buckets.push(Bucket {
            scope: "session",
            start: started,
            end: ending.then_some(timestamp),
            session: Some(run.into()),
            cumulative,
        });
        buckets.push(Bucket {
            scope: "trip",
            start: trip.started_ms,
            end: trip.ended_ms,
            session: Some(trip.id.clone()),
            cumulative: Some((trip.active_s, trip.distance_m, trip.calories_kcal)),
        });
        for bucket in buckets {
            upsert(tx, mysql, device, demo, timestamp, bucket, &totals).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_calendar_boundaries_are_stable() {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 59)
            .single()
            .unwrap()
            .timestamp_millis();
        let buckets = calendar(timestamp);
        let month = buckets.iter().find(|b| b.scope == "month").unwrap();
        assert_eq!(
            month.end,
            Some(
                Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0)
                    .single()
                    .unwrap()
                    .timestamp_millis()
            )
        );
        let week = buckets.iter().find(|b| b.scope == "week").unwrap();
        assert_eq!(
            Utc.timestamp_millis_opt(week.start).unwrap().weekday(),
            chrono::Weekday::Mon
        );
        let next = calendar(timestamp + 1000);
        assert_ne!(
            buckets.iter().find(|b| b.scope == "day").unwrap().start,
            next.iter().find(|b| b.scope == "day").unwrap().start
        );
    }
}
