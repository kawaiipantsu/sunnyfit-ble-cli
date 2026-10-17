//! Workout-only features. The headless connector never constructs this engine.
use crate::{
    config::{self, Config},
    metrics::Workout,
};
use anyhow::{bail, Context, Result};
use chrono::{Datelike, Local};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntervalPlan {
    pub name: String,
    #[serde(default = "one")]
    pub repeats: u32,
    pub steps: Vec<Step>,
}
fn one() -> u32 {
    1
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    pub seconds: u64,
    pub cadence_min: f64,
    pub cadence_max: f64,
}
impl Default for IntervalPlan {
    fn default() -> Self {
        Self {
            name: "Desk rhythm".into(),
            repeats: 3,
            steps: vec![
                Step {
                    name: "Comfortable spin".into(),
                    seconds: 120,
                    cadence_min: 40.,
                    cadence_max: 65.,
                },
                Step {
                    name: "Lift the rhythm".into(),
                    seconds: 60,
                    cadence_min: 65.,
                    cadence_max: 85.,
                },
                Step {
                    name: "Recovery".into(),
                    seconds: 60,
                    cadence_min: 25.,
                    cadence_max: 45.,
                },
            ],
        }
    }
}
impl IntervalPlan {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let p: Self = if let Some(path) = path {
            toml::from_str(&fs::read_to_string(path)?)
                .map_err(|_| anyhow::anyhow!("invalid interval plan"))?
        } else {
            Self::default()
        };
        if p.name.is_empty()
            || p.repeats == 0
            || p.repeats > 1000
            || p.steps.is_empty()
            || p.steps.len() > 1000
        {
            bail!("interval plan requires a name, 1–1000 repeats and 1–1000 steps");
        }
        for s in &p.steps {
            if s.seconds == 0
                || s.seconds > 86400
                || !s.cadence_min.is_finite()
                || !s.cadence_max.is_finite()
                || s.cadence_min < 0.
                || s.cadence_max < s.cadence_min
                || s.cadence_max > 300.
            {
                bail!("invalid interval duration or cadence range");
            }
        }
        Ok(p)
    }
    pub fn duration(&self) -> f64 {
        self.steps.iter().map(|s| s.seconds as f64).sum::<f64>() * self.repeats as f64
    }
    pub fn current(&self, elapsed: f64) -> Option<(usize, usize, &Step, f64)> {
        if elapsed >= self.duration() {
            return None;
        }
        let cycle = self.duration() / self.repeats as f64;
        let lap = (elapsed / cycle).floor() as usize;
        let mut within = elapsed % cycle;
        for (i, s) in self.steps.iter().enumerate() {
            if within < (s.seconds as f64) {
                return Some((lap, i, s, s.seconds as f64 - within));
            }
            within -= s.seconds as f64;
        }
        None
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Contribution {
    pub active_s: f64,
    pub distance_m: f64,
    pub route: String,
    pub days: BTreeMap<String, f64>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    pub runs: BTreeMap<String, Contribution>,
}
impl Progress {
    pub fn active(&self) -> f64 {
        self.runs.values().map(|r| r.active_s).sum()
    }
    pub fn distance(&self) -> f64 {
        self.runs.values().map(|r| r.distance_m).sum()
    }
    pub fn daily(&self, day: &str) -> f64 {
        self.runs
            .values()
            .map(|r| r.days.get(day).copied().unwrap_or(0.))
            .sum()
    }
    pub fn xp(&self) -> u64 {
        (self.active() / 60. * 10. + self.distance() / 1000. * 25.).floor() as u64
    }
    pub fn achievements(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("First turns", self.active() >= 10.),
            ("Desk regular · 30 min", self.active() >= 1800.),
            ("Five kilometre club", self.distance() >= 5000.),
            ("Century · 100 km", self.distance() >= 100000.),
            (
                "Summit complete",
                self.runs
                    .values()
                    .filter(|r| r.route == "summit")
                    .map(|r| r.distance_m)
                    .sum::<f64>()
                    >= 12000.,
            ),
        ]
    }
}
pub struct Route {
    pub name: &'static str,
    pub length_m: f64,
    pub stops: &'static [(&'static str, f64)],
}
pub fn route(id: &str) -> Route {
    match id {
        "route66" => Route {
            name: "Route 66",
            length_m: 3940000.,
            stops: &[
                ("Chicago", 0.),
                ("St Louis", 480000.),
                ("Oklahoma City", 1260000.),
                ("Amarillo", 1700000.),
                ("Albuquerque", 2170000.),
                ("Flagstaff", 2710000.),
                ("Santa Monica", 3940000.),
            ],
        },
        "summit" => Route {
            name: "Cloudline summit",
            length_m: 12000.,
            stops: &[
                ("Trailhead", 0.),
                ("Pine forest", 3000.),
                ("Alpine lake", 6500.),
                ("High ridge", 9500.),
                ("Summit", 12000.),
            ],
        },
        _ => Route {
            name: "Across Denmark",
            length_m: 430000.,
            stops: &[
                ("Copenhagen", 0.),
                ("Roskilde", 35000.),
                ("Odense", 165000.),
                ("Aarhus", 300000.),
                ("Aalborg", 430000.),
            ],
        },
    }
}
#[derive(Default)]
pub struct Ghost {
    pub label: String,
    pub points: Vec<(f64, f64)>,
}
impl Ghost {
    pub fn load(selection: Option<&str>, base: &Path) -> Result<Self> {
        let Some(selection) = selection else {
            return Ok(Self::default());
        };
        let path = if matches!(selection, "latest" | "best") {
            let mut choices = Vec::new();
            if base.exists() {
                for entry in fs::read_dir(base)? {
                    let dir = entry?.path();
                    let summary = dir.join("summary.json");
                    if !summary.is_file() {
                        continue;
                    }
                    let v: Value = serde_json::from_slice(&fs::read(summary)?)?;
                    if v["demo"] == true {
                        continue;
                    }
                    let distance = v["workout"]["distance_m"].as_f64().unwrap_or(0.);
                    if distance > 0. {
                        choices.push((dir, distance));
                    }
                }
            }
            if selection == "best" {
                choices.sort_by(|a, b| a.1.total_cmp(&b.1));
            } else {
                choices.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let Some((dir, _)) = choices.pop() else {
                return Ok(Self::default());
            };
            dir.join("events.jsonl")
        } else {
            let p = PathBuf::from(selection);
            if p.is_dir() {
                p.join("events.jsonl")
            } else {
                p
            }
        };
        let mut points = vec![(0., 0.)];
        for line in BufReader::new(fs::File::open(path)?).lines() {
            let row: Value = serde_json::from_str(&line?)?;
            if row["type"] != "metrics" {
                continue;
            }
            if let (Some(t), Some(d)) = (
                row["workout"]["active_s"].as_f64(),
                row["workout"]["distance_m"].as_f64(),
            ) {
                if t.is_finite()
                    && d.is_finite()
                    && t > points.last().unwrap().0
                    && d >= points.last().unwrap().1
                {
                    points.push((t, d));
                }
            }
        }
        if points.len() < 2 {
            bail!("ghost needs at least two valid timed distance samples");
        }
        Ok(Self {
            label: if matches!(selection, "latest" | "best") {
                selection.into()
            } else {
                "Selected workout".into()
            },
            points,
        })
    }
    pub fn distance_at(&self, t: f64) -> Option<f64> {
        if self.points.len() < 2 {
            return None;
        }
        if t > self.points.last()?.0 {
            return Some(self.points.last()?.1);
        }
        let i = self
            .points
            .partition_point(|p| p.0 < t)
            .clamp(1, self.points.len() - 1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        Some(a.1 + (b.1 - a.1) * ((t - a.0) / (b.0 - a.0)).clamp(0., 1.))
    }
}
pub struct Training {
    pub plan: IntervalPlan,
    pub interval_s: f64,
    pub in_target_s: f64,
    pub ghost: Ghost,
    pub progress: Progress,
    pub route_distance: f64,
    pub toast: String,
    pub focus_s: f64,
    pub focus_paused: bool,
    pub game_s: f64,
    pub gate_score: u64,
    pub flight_score: u64,
    pub flight_misses: u64,
    path: PathBuf,
    run_id: String,
    demo: bool,
    last_ms: Option<u64>,
    last_active: f64,
    last_persist: u64,
    last_movement: f64,
    low_since: Option<f64>,
    last_alert: f64,
    last_stage: Option<(usize, usize)>,
    last_gate: u64,
    last_flight: u64,
    last_focus: u64,
    route_prior: f64,
}
impl Training {
    pub fn new(c: &Config, dir: &Path, run_id: &str, demo: bool, history: &Path) -> Result<Self> {
        let path = dir.join("progress.json");
        let progress = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?).context("invalid workout progress file")?
        } else {
            Progress::default()
        };
        let prior = progress
            .runs
            .values()
            .filter(|r| r.route == c.route)
            .map(|r| r.distance_m)
            .sum();
        Ok(Self {
            plan: IntervalPlan::load(c.interval_plan.as_deref())?,
            interval_s: 0.,
            in_target_s: 0.,
            ghost: Ghost::load(c.ghost.as_deref(), history)?,
            progress,
            route_distance: prior,
            toast: "Find a comfortable rhythm. Every turn counts.".into(),
            focus_s: 0.,
            focus_paused: false,
            game_s: 0.,
            gate_score: 0,
            flight_score: 0,
            flight_misses: 0,
            path,
            run_id: run_id.into(),
            demo,
            last_ms: None,
            last_active: 0.,
            last_persist: 0,
            last_movement: 0.,
            low_since: None,
            last_alert: 0.,
            last_stage: None,
            last_gate: 0,
            last_flight: 0,
            last_focus: 0,
            route_prior: prior,
        })
    }
    fn notify(&mut self, c: &Config, message: String) {
        self.toast = message;
        if c.notifications {
            let _ = std::process::Command::new("notify-send")
                .arg("SunnyFit")
                .arg(&self.toast)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|mut child| {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    })
                });
        }
    }
    pub fn tick(&mut self, w: &Workout, c: &Config, now: u64) -> Result<()> {
        let dt = self
            .last_ms
            .map(|last| now.saturating_sub(last) as f64 / 1000.)
            .unwrap_or(0.)
            .min(1.);
        self.last_ms = Some(now);
        let live = !w.paused && !w.stale && w.connection == "ready";
        let active_delta = (w.active_s - self.last_active).max(0.);
        self.last_active = w.active_s;
        let day = Local::now().format("%Y-%m-%d").to_string();
        let run = self.progress.runs.entry(self.run_id.clone()).or_default();
        run.active_s = w.active_s;
        run.distance_m = w.distance_m.unwrap_or(0.);
        run.route = c.route.clone();
        *run.days.entry(day).or_default() += active_delta;
        self.route_distance = self.route_prior + w.distance_m.unwrap_or(0.);
        if live {
            self.interval_s += dt;
            if let Some((lap, index, step, _)) = self.plan.current(self.interval_s) {
                if w.current
                    .cadence_rpm
                    .is_some_and(|rpm| rpm >= step.cadence_min && rpm <= step.cadence_max)
                {
                    self.in_target_s += dt;
                }
                let stage = (lap, index);
                if self.last_stage != Some(stage) {
                    let msg = format!(
                        "{} · {:.0}–{:.0} rpm",
                        step.name, step.cadence_min, step.cadence_max
                    );
                    self.last_stage = Some(stage);
                    self.notify(c, msg);
                }
            } else if self.last_stage.take().is_some() {
                self.notify(c, "Interval plan complete. Enjoy an easy cooldown.".into());
            }
            self.game_s += dt;
            let gate = (self.game_s / 8.) as u64;
            if gate > self.last_gate {
                let target = Self::gate_target(gate - 1);
                if w.current
                    .cadence_rpm
                    .is_some_and(|rpm| (rpm - target).abs() <= 10.)
                {
                    self.gate_score += 100;
                }
                self.last_gate = gate;
            }
            let flight = (self.game_s / 6.) as u64;
            if flight > self.last_flight {
                let target = Self::flight_target(flight - 1);
                if w.current
                    .cadence_rpm
                    .is_some_and(|rpm| (rpm - target).abs() <= 15.)
                {
                    self.flight_score += 1;
                } else {
                    self.flight_misses += 1;
                }
                self.last_flight = flight;
            }
        }
        if active_delta > 0. {
            self.last_movement = w.elapsed_s;
        }
        if w.elapsed_s - self.last_movement >= c.movement_reminder_minutes as f64 * 60. {
            self.last_movement = w.elapsed_s;
            self.notify(
                c,
                "Movement break: a few comfortable turns can brighten your desk session.".into(),
            );
        }
        if live
            && w.current
                .cadence_rpm
                .is_some_and(|r| r < c.cadence_alert_rpm)
        {
            let since = *self.low_since.get_or_insert(w.elapsed_s);
            if w.elapsed_s - since >= c.cadence_alert_seconds as f64
                && w.elapsed_s - self.last_alert >= c.cadence_alert_seconds as f64
            {
                self.last_alert = w.elapsed_s;
                self.notify(
                    c,
                    "Cadence is below your desk target. Move when it feels comfortable.".into(),
                );
            }
        } else {
            self.low_since = None;
        }
        if !self.focus_paused {
            self.focus_s += dt;
        }
        let cycle = (c.focus_minutes + c.break_minutes) as f64 * 60.;
        let segment = (self.focus_s / cycle).floor() as u64 * 2
            + u64::from(self.focus_s % cycle >= c.focus_minutes as f64 * 60.);
        if segment != self.last_focus {
            self.last_focus = segment;
            self.notify(
                c,
                if segment.is_multiple_of(2) {
                    "Focus block starts. Settle into a comfortable rhythm."
                } else {
                    "Time for a break. Look away from the screen and stretch."
                }
                .into(),
            );
        }
        if now.saturating_sub(self.last_persist) >= 30000 {
            self.persist()?;
            self.last_persist = now;
        }
        Ok(())
    }
    pub fn gate_target(gate: u64) -> f64 {
        [40., 60., 50., 75.][gate as usize % 4]
    }
    pub fn flight_target(gate: u64) -> f64 {
        [45., 70., 55., 80., 40.][gate as usize % 5]
    }
    pub fn daily_target() -> u64 {
        15 + (Local::now().ordinal() as u64 % 5) * 5
    }
    pub fn persist(&mut self) -> Result<()> {
        if self.demo {
            return Ok(());
        }
        let parent = self.path.parent().context("progress directory")?;
        config::private_dir(parent)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(parent.join("progress.lock"))?;
        lock.lock_exclusive()?;
        let mut merged: Progress = if self.path.exists() {
            serde_json::from_slice(&fs::read(&self.path)?)?
        } else {
            Progress::default()
        };
        if let Some(run) = self.progress.runs.get(&self.run_id) {
            merged.runs.insert(self.run_id.clone(), run.clone());
        }
        config::write_private(&self.path, &serde_json::to_vec_pretty(&merged)?)?;
        self.progress = merged;
        FileExt::unlock(&lock)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interval_boundaries_and_completion() {
        let p = IntervalPlan::default();
        assert_eq!(p.current(120.).unwrap().2.name, "Lift the rhythm");
        assert_eq!(p.current(240.).unwrap().0, 1);
        assert!(p.current(720.).is_none());
    }
    #[test]
    fn ghost_interpolates_and_holds_finish() {
        let g = Ghost {
            label: "test".into(),
            points: vec![(0., 0.), (10., 100.), (20., 150.)],
        };
        assert_eq!(g.distance_at(5.), Some(50.));
        assert_eq!(g.distance_at(100.), Some(150.));
    }
    #[test]
    fn disconnected_freezes_training_and_demo_does_not_persist() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config::default();
        let mut t = Training::new(&c, &dir, "test", true, &dir).unwrap();
        let mut w = Workout::default();
        t.tick(&w, &c, 0).unwrap();
        t.tick(&w, &c, 1000).unwrap();
        assert_eq!(t.interval_s, 0.);
        w.connection = "ready".into();
        w.stale = false;
        w.current.cadence_rpm = Some(60.);
        t.tick(&w, &c, 2000).unwrap();
        assert_eq!(t.interval_s, 1.);
        w.paused = true;
        t.tick(&w, &c, 3000).unwrap();
        assert_eq!(t.interval_s, 1.);
        t.persist().unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn progress_merge_is_idempotent_and_retains_other_runs() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let c = Config::default();
        let mut a = Training::new(&c, &dir, "a", false, &dir).unwrap();
        let mut b = Training::new(&c, &dir, "b", false, &dir).unwrap();
        a.progress.runs.insert(
            "a".into(),
            Contribution {
                active_s: 60.,
                distance_m: 100.,
                ..Default::default()
            },
        );
        b.progress.runs.insert(
            "b".into(),
            Contribution {
                active_s: 120.,
                distance_m: 200.,
                ..Default::default()
            },
        );
        a.persist().unwrap();
        b.persist().unwrap();
        a.persist().unwrap();
        let p: Progress =
            serde_json::from_slice(&fs::read(dir.join("progress.json")).unwrap()).unwrap();
        assert_eq!(p.runs.len(), 2);
        assert_eq!(p.active(), 180.);
        assert_eq!(p.distance(), 300.);
        fs::remove_dir_all(dir).unwrap();
    }
}
