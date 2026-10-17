//! Speed records from available saved measurements; demos never enter real history.
use crate::metrics::Workout;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SpeedTotals {
    pub minimum_kmh: Option<f64>,
    pub maximum_kmh: Option<f64>,
    pub integral: f64,
    pub observed_s: f64,
}
impl SpeedTotals {
    pub fn average(&self) -> Option<f64> {
        (self.observed_s > 0.).then(|| self.integral / self.observed_s)
    }
    pub fn merge(&mut self, other: &Self) {
        if let Some(v) = other.minimum_kmh {
            self.minimum_kmh = Some(self.minimum_kmh.map_or(v, |old| old.min(v)));
        }
        if let Some(v) = other.maximum_kmh {
            self.maximum_kmh = Some(self.maximum_kmh.map_or(v, |old| old.max(v)));
        }
        self.integral += other.integral;
        self.observed_s += other.observed_s;
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LifetimeSpeed {
    pub source: String,
    pub totals: SpeedTotals,
}
pub fn local_history(base: &Path) -> SpeedTotals {
    let mut totals = SpeedTotals::default();
    let Ok(entries) = fs::read_dir(base) else {
        return totals;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(summary) = fs::read(path.join("summary.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        if summary["demo"] == true {
            continue;
        }
        if let Ok(w) = serde_json::from_value::<Workout>(summary["workout"].clone()) {
            if w.speed_totals().observed_s > 0. {
                totals.merge(&w.speed_totals());
                continue;
            }
        }
        // Version 0.4 stored exact active-time weights in metric contributions.
        let mut run = SpeedTotals::default();
        if let Ok(file) = fs::File::open(path.join("events.jsonl")) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                let Ok(row) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let d = &row["statistics"]["delta"];
                let seconds = d["speed_observed_s"].as_f64().unwrap_or(0.);
                if seconds > 0. {
                    run.merge(&SpeedTotals {
                        minimum_kmh: d["speed_min_kmh"].as_f64(),
                        maximum_kmh: d["speed_max_kmh"].as_f64(),
                        integral: d["speed_integral"].as_f64().unwrap_or(0.),
                        observed_s: seconds,
                    });
                }
            }
        }
        // Older summaries can still contribute a known peak, without inventing weights/minima.
        if run.maximum_kmh.is_none() {
            run.maximum_kmh = summary["workout"]["max_speed_kmh"].as_f64();
        }
        totals.merge(&run);
    }
    totals
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_speed_records_use_weights_and_exclude_demo() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&dir).unwrap();
        for (id, speed, seconds, demo) in [
            ("one", 10., 10., false),
            ("two", 30., 30., false),
            ("demo", 100., 500., true),
        ] {
            let path = dir.join(id);
            fs::create_dir(&path).unwrap();
            let mut w = Workout::default();
            w.connection("ready");
            w.ingest(
                crate::metrics::Sample {
                    speed_kmh: Some(speed),
                    ..Default::default()
                },
                0,
                1,
                &crate::config::Config::default(),
            );
            let c = crate::config::Config {
                stale_seconds: 1000,
                ..Default::default()
            };
            w.tick((seconds * 1000.) as u64, &c);
            fs::write(
                path.join("summary.json"),
                serde_json::to_vec(&serde_json::json!({"demo":demo,"workout":w})).unwrap(),
            )
            .unwrap();
        }
        let history = local_history(&dir);
        assert_eq!(history.minimum_kmh, Some(10.));
        assert_eq!(history.maximum_kmh, Some(30.));
        assert_eq!(history.average(), Some(25.));
        fs::remove_dir_all(dir).unwrap();
    }
}
