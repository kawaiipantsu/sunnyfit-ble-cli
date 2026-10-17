//! Indoor activity export. Never invent GPS, heart rate, or power.
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};
#[derive(Clone)]
pub struct Point {
    pub timestamp: i64,
    pub elapsed: f64,
    pub active: f64,
    pub distance: Option<f64>,
    pub speed: Option<f64>,
    pub cadence: Option<f64>,
    pub heart: Option<f64>,
    pub power: Option<f64>,
}
pub struct Activity {
    pub id: String,
    pub start: i64,
    pub end: i64,
    pub elapsed: f64,
    pub active: f64,
    pub distance: Option<f64>,
    pub calories: Option<f64>,
    pub points: Vec<Point>,
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite() && *x >= 0.)
}
pub fn read(path: &Path) -> Result<Vec<Value>> {
    let path = if path.is_dir() {
        path.join("events.jsonl")
    } else {
        path.to_owned()
    };
    BufReader::new(fs::File::open(path)?)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}
impl Activity {
    pub fn from_rows(rows: &[Value]) -> Result<Self> {
        let mut points = Vec::new();
        let mut id = None;
        let mut start = None;
        let mut final_workout = None;
        for row in rows
            .iter()
            .filter(|r| matches!(r["type"].as_str(), Some("metrics" | "workout_summary")))
        {
            let row_id = row["run_id"]
                .as_str()
                .context("export event lacks session ID")?;
            if id.is_some_and(|id| id != row_id) {
                bail!("export input contains multiple sessions");
            }
            id = Some(row_id);
            let timestamp = row["timestamp_unix_ms"]
                .as_i64()
                .context("export needs wall-clock timestamps")?;
            let elapsed = row["elapsed_ms"]
                .as_i64()
                .context("export needs elapsed time")?;
            start.get_or_insert(timestamp - elapsed);
            let w = &row["workout"];
            let s = &w["current"];
            final_workout = Some(w);
            if row["type"] == "metrics" {
                points.push(Point {
                    timestamp,
                    elapsed: elapsed as f64 / 1000.,
                    active: number(&w["active_s"]).unwrap_or(0.),
                    distance: number(&w["distance_m"]),
                    speed: number(&s["speed_kmh"]).map(|v| v / 3.6),
                    cadence: number(&s["cadence_rpm"]),
                    heart: number(&s["heart_rate_bpm"]).filter(|v| *v > 0.),
                    power: number(&s["power_w"]),
                });
            }
        }
        if points.is_empty() {
            bail!("session has no timed metrics to export");
        }
        if points.windows(2).any(|p| p[1].timestamp < p[0].timestamp) {
            bail!("session clock moved backwards; cannot export a coherent timeline");
        }
        let w = final_workout.unwrap();
        let start = start.unwrap();
        let elapsed = number(&w["elapsed_s"]).unwrap_or(points.last().unwrap().elapsed);
        Ok(Self {
            id: id.unwrap().into(),
            start,
            end: start + (elapsed * 1000.) as i64,
            elapsed,
            active: number(&w["active_s"]).unwrap_or(0.),
            distance: number(&w["distance_m"]),
            calories: number(&w["calories_kcal"]),
            points,
        })
    }
    pub fn encode(&self, format: &str) -> Result<Vec<u8>> {
        match format {
            "csv" => self.csv(),
            "tcx" => self.tcx(),
            "fit" => self.fit(),
            _ => bail!("format must be csv, tcx or fit"),
        }
    }
    fn csv(&self) -> Result<Vec<u8>> {
        let mut w = csv::Writer::from_writer(Vec::new());
        w.write_record([
            "timestamp_utc",
            "elapsed_s",
            "active_s",
            "distance_m",
            "speed_m_s",
            "cadence_rpm",
            "heart_rate_bpm",
            "power_w",
            "activity_type",
        ])?;
        for p in &self.points {
            w.write_record([
                iso(p.timestamp)?,
                p.elapsed.to_string(),
                p.active.to_string(),
                opt(p.distance),
                opt(p.speed),
                opt(p.cadence),
                opt(p.heart),
                opt(p.power),
                "indoor_cycling".into(),
            ])?;
        }
        Ok(w.into_inner()?)
    }
    fn tcx(&self) -> Result<Vec<u8>> {
        let start = iso(self.start)?;
        let mut text=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<TrainingCenterDatabase xmlns=\"http://www.garmin.com/xmlschemas/TrainingCenterDatabase/v2\" xmlns:ns3=\"http://www.garmin.com/xmlschemas/ActivityExtension/v2\"><Activities><Activity Sport=\"Biking\"><Id>{start}</Id><Lap StartTime=\"{start}\"><TotalTimeSeconds>{:.3}</TotalTimeSeconds><DistanceMeters>{:.3}</DistanceMeters><Calories>{}</Calories><Intensity>Active</Intensity><TriggerMethod>Manual</TriggerMethod><Track>",self.elapsed,self.distance.unwrap_or(0.),self.calories.unwrap_or(0.).round().min(65535.)as u16);
        for p in &self.points {
            text.push_str(&format!("<Trackpoint><Time>{}</Time>", iso(p.timestamp)?));
            if let Some(d) = p.distance {
                text.push_str(&format!("<DistanceMeters>{d:.3}</DistanceMeters>"));
            }
            if let Some(h) = p.heart.filter(|v| *v < 256.) {
                text.push_str(&format!(
                    "<HeartRateBpm><Value>{}</Value></HeartRateBpm>",
                    h.round() as u16
                ));
            }
            if let Some(c) = p.cadence.filter(|v| *v < 255.) {
                text.push_str(&format!("<Cadence>{}</Cadence>", c.round() as u16));
            }
            if p.speed.is_some() || p.power.is_some() {
                text.push_str("<Extensions><ns3:TPX CadenceSensor=\"Bike\">");
                if let Some(s) = p.speed {
                    text.push_str(&format!("<ns3:Speed>{s:.4}</ns3:Speed>"));
                }
                if let Some(power) = p.power.filter(|v| *v < 65536.) {
                    text.push_str(&format!("<ns3:Watts>{}</ns3:Watts>", power.round() as u16));
                }
                text.push_str("</ns3:TPX></Extensions>");
            }
            text.push_str("</Trackpoint>\n");
        }
        text.push_str("</Track></Lap><Notes>Indoor cycling; no GPS. Missing heart rate and power are omitted. Lap distance/calories use zero only when unavailable, as required by TCX; energy may be estimated.</Notes></Activity></Activities></TrainingCenterDatabase>\n");
        Ok(text.into_bytes())
    }
    pub fn fit(&self) -> Result<Vec<u8>> {
        let start = fit_time(self.start)?;
        let end = fit_time(self.end)?;
        let mut data = Vec::new();
        message(
            &mut data,
            0,
            &[u8f(0, 4, 0), u16f(1, 255), u16f(2, 0), u32f(4, start)],
        );
        message(
            &mut data,
            21,
            &[u32f(253, start), u8f(0, 0, 0), u8f(1, 0, 0)],
        );
        for p in &self.points {
            let mut fields = vec![u32f(253, fit_time(p.timestamp)?)];
            add(&mut fields, 5, p.distance, 100., 4);
            add(&mut fields, 6, p.speed, 1000., 2);
            add(&mut fields, 4, p.cadence, 1., 1);
            add(&mut fields, 3, p.heart, 1., 1);
            add(&mut fields, 7, p.power, 1., 2);
            message(&mut data, 20, &fields);
        }
        message(&mut data, 21, &[u32f(253, end), u8f(0, 0, 0), u8f(1, 4, 0)]);
        let mut totals = vec![
            u16f(254, 0),
            u32f(253, end),
            u32f(2, start),
            u32f(7, (self.elapsed * 1000.).round() as u32),
            u32f(8, (self.active * 1000.).round() as u32),
        ];
        add(&mut totals, 9, self.distance, 100., 4);
        add(&mut totals, 11, self.calories, 1., 2);
        let mut lap = totals.clone();
        lap.extend([u8f(0, 9, 0), u8f(1, 1, 0), u8f(25, 2, 0)]);
        message(&mut data, 19, &lap);
        let mut session = totals;
        session.extend([
            u8f(0, 8, 0),
            u8f(1, 1, 0),
            u8f(5, 2, 0),
            u8f(6, 6, 0),
            u16f(25, 0),
            u16f(26, 1),
        ]);
        message(&mut data, 18, &session);
        message(
            &mut data,
            34,
            &[
                u32f(253, end),
                u32f(0, (self.active * 1000.).round() as u32),
                u16f(1, 1),
                u8f(2, 0, 0),
                u8f(3, 26, 0),
                u8f(4, 1, 0),
            ],
        );
        let mut file = vec![14, 0x10];
        file.extend(2184u16.to_le_bytes());
        file.extend((data.len() as u32).to_le_bytes());
        file.extend(b".FIT");
        file.extend(crc(&file).to_le_bytes());
        file.extend(data);
        file.extend(crc(&file).to_le_bytes());
        Ok(file)
    }
}
fn iso(ms: i64) -> Result<String> {
    Ok(DateTime::<Utc>::from_timestamp_millis(ms)
        .context("invalid activity timestamp")?
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}
fn opt(v: Option<f64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}
fn fit_time(ms: i64) -> Result<u32> {
    let t = ms / 1000 - 631065600;
    if !(0..u32::MAX as i64).contains(&t) {
        bail!("timestamp outside FIT range");
    }
    Ok(t as u32)
}
#[derive(Clone)]
struct Field {
    number: u8,
    base: u8,
    bytes: Vec<u8>,
}
fn u8f(n: u8, v: u8, base: u8) -> Field {
    Field {
        number: n,
        base,
        bytes: vec![v],
    }
}
fn u16f(n: u8, v: u16) -> Field {
    Field {
        number: n,
        base: 0x84,
        bytes: v.to_le_bytes().to_vec(),
    }
}
fn u32f(n: u8, v: u32) -> Field {
    Field {
        number: n,
        base: 0x86,
        bytes: v.to_le_bytes().to_vec(),
    }
}
fn add(fields: &mut Vec<Field>, n: u8, value: Option<f64>, scale: f64, width: usize) {
    if let Some(v) = value {
        let raw = (v * scale).round();
        let max = match width {
            1 => 254.,
            2 => 65534.,
            _ => 4294967294.,
        };
        if raw.is_finite() && raw >= 0. && raw <= max {
            fields.push(match width {
                1 => u8f(n, raw as u8, 2),
                2 => u16f(n, raw as u16),
                _ => u32f(n, raw as u32),
            });
        }
    }
}
fn message(data: &mut Vec<u8>, global: u16, fields: &[Field]) {
    data.extend([0x40, 0, 0]);
    data.extend(global.to_le_bytes());
    data.push(fields.len() as u8);
    for f in fields {
        data.extend([f.number, f.bytes.len() as u8, f.base]);
    }
    data.push(0);
    for f in fields {
        data.extend(&f.bytes);
    }
}
fn crc(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in bytes {
        crc ^= *b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xa001
            } else {
                crc >> 1
            };
        }
    }
    crc
}
pub fn save(path: &Path, data: &[u8]) -> Result<()> {
    crate::config::write_private(path, data)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exports_omit_unavailable_metrics() {
        let rows = vec![
            json!({"type":"metrics","run_id":"test","timestamp_unix_ms":1700000000000_i64,"elapsed_ms":1000,"workout":{"elapsed_s":1.,"active_s":1.,"current":{"cadence_rpm":60.}}}),
        ];
        let a = Activity::from_rows(&rows).unwrap();
        let csv = String::from_utf8(a.csv().unwrap()).unwrap();
        assert!(csv.contains(",60,,,indoor_cycling"));
        let tcx = String::from_utf8(a.tcx().unwrap()).unwrap();
        assert!(!tcx.contains("<HeartRateBpm>"));
        assert!(!tcx.contains("<ns3:Watts>"));
        let fit = a.fit().unwrap();
        assert_eq!(crc(&fit), 0);
        assert_eq!(&fit[8..12], b".FIT");
    }
}
