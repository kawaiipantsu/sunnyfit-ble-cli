//! One-second observations become additive contributions, plus inactivity-bounded trips.
use crate::{config::Config, metrics::Workout};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Totals {
    pub samples: i64,
    pub elapsed_s: f64,
    pub active_s: f64,
    pub trip_elapsed_s: f64,
    pub distance_m: Option<f64>,
    pub calories_kcal: Option<f64>,
    pub speed_min_kmh: Option<f64>,
    pub speed_max_kmh: Option<f64>,
    pub speed_integral: f64,
    pub speed_observed_s: f64,
    pub cadence_min_rpm: Option<f64>,
    pub cadence_max_rpm: Option<f64>,
    pub cadence_integral: f64,
    pub cadence_observed_s: f64,
    pub heart_rate_min_bpm: Option<f64>,
    pub heart_rate_max_bpm: Option<f64>,
    pub heart_rate_integral: f64,
    pub heart_rate_observed_s: f64,
    pub power_min_w: Option<f64>,
    pub power_max_w: Option<f64>,
    pub power_integral: f64,
    pub power_observed_s: f64,
    pub sessions_started: i64,
    pub trips_started: i64,
    pub trips_completed: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Trip {
    pub id: String,
    pub started_ms: i64,
    pub last_moving_ms: i64,
    pub last_moving_elapsed_ms: u64,
    pub ended_ms: Option<i64>,
    pub end_reason: Option<String>,
    pub elapsed_s: f64,
    pub active_s: f64,
    pub distance_m: Option<f64>,
    pub calories_kcal: Option<f64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Observation {
    pub delta: Totals,
    pub trip: Option<Trip>,
    pub closed_trip: Option<Trip>,
}
#[derive(Default)]
pub struct Tracker {
    previous: Option<(u64, f64, Option<f64>, Option<f64>)>,
    trip: Option<Trip>,
}
fn difference(value: Option<f64>, previous: Option<f64>) -> Option<f64> {
    value.map(|v| (v - previous.unwrap_or(0.)).max(0.))
}
fn add(total: &mut Option<f64>, value: Option<f64>) {
    if let Some(value) = value {
        *total.get_or_insert(0.) += value;
    }
}
impl Tracker {
    pub fn observe(
        &mut self,
        w: &Workout,
        c: &Config,
        elapsed_ms: u64,
        timestamp_ms: i64,
        ending: bool,
    ) -> Observation {
        let previous = self.previous.unwrap_or((0, 0., None, None));
        let mut delta = Totals {
            samples: i64::from(!ending),
            elapsed_s: elapsed_ms.saturating_sub(previous.0) as f64 / 1000.,
            active_s: (w.active_s - previous.1).max(0.),
            distance_m: difference(w.distance_m, previous.2),
            calories_kcal: difference(w.calories_kcal, previous.3),
            sessions_started: i64::from(self.previous.is_none()),
            ..Default::default()
        };
        let moving = w.connection == "ready"
            && !w.stale
            && !w.paused
            && (w.current.speed_kmh.is_some_and(|v| v > 0.)
                || w.current.cadence_rpm.is_some_and(|v| v > 0.));
        let observed_motion = moving || delta.active_s > 0.;
        // Averages are weighted by active seconds with available measurements.
        macro_rules! measure {
            ($value:expr,$min:ident,$max:ident,$sum:ident,$time:ident) => {
                if w.connection == "ready" && !w.stale && !w.paused {
                    if let Some(v) = $value.filter(|v| v.is_finite()) {
                        if delta.active_s > 0. {
                            delta.$min = Some(v);
                            delta.$max = Some(v);
                            delta.$sum = v * delta.active_s;
                            delta.$time = delta.active_s;
                        }
                    }
                }
            };
        }
        measure!(
            w.current.speed_kmh,
            speed_min_kmh,
            speed_max_kmh,
            speed_integral,
            speed_observed_s
        );
        measure!(
            w.current.cadence_rpm,
            cadence_min_rpm,
            cadence_max_rpm,
            cadence_integral,
            cadence_observed_s
        );
        measure!(
            w.current.heart_rate_bpm,
            heart_rate_min_bpm,
            heart_rate_max_bpm,
            heart_rate_integral,
            heart_rate_observed_s
        );
        measure!(
            w.current.power_w,
            power_min_w,
            power_max_w,
            power_integral,
            power_observed_s
        );
        let mut closed_trip = None;
        if c.trip_idle_seconds > 0 {
            if self.trip.as_ref().is_some_and(|t| {
                elapsed_ms.saturating_sub(t.last_moving_elapsed_ms) >= c.trip_idle_seconds * 1000
            }) {
                let mut trip = self.trip.take().unwrap();
                trip.ended_ms = Some(trip.last_moving_ms);
                trip.end_reason = Some("idle_timeout".into());
                closed_trip = Some(trip);
            }
            if observed_motion && self.trip.is_none() && !ending {
                let started_ms = timestamp_ms - (delta.active_s * 1000.).round() as i64;
                self.trip = Some(Trip {
                    id: uuid::Uuid::new_v4().to_string(),
                    started_ms,
                    last_moving_ms: timestamp_ms,
                    last_moving_elapsed_ms: elapsed_ms,
                    ended_ms: None,
                    end_reason: None,
                    elapsed_s: 0.,
                    active_s: 0.,
                    distance_m: None,
                    calories_kcal: None,
                });
                delta.trips_started = 1;
            }
            if let Some(trip) = &mut self.trip {
                let previous_elapsed = trip.elapsed_s;
                if observed_motion {
                    trip.last_moving_ms = timestamp_ms;
                    trip.last_moving_elapsed_ms = elapsed_ms;
                    trip.elapsed_s = (timestamp_ms - trip.started_ms).max(0) as f64 / 1000.;
                }
                delta.trip_elapsed_s = (trip.elapsed_s - previous_elapsed).max(0.);
                trip.active_s += delta.active_s;
                add(&mut trip.distance_m, delta.distance_m);
                add(&mut trip.calories_kcal, delta.calories_kcal);
            }
        }
        let trip = self.trip.clone();
        if ending {
            if let Some(mut trip) = self.trip.take() {
                trip.ended_ms = Some(trip.last_moving_ms);
                trip.end_reason = Some("collector_stopped".into());
                closed_trip = Some(trip);
            }
        }
        self.previous = Some((elapsed_ms, w.active_s, w.distance_m, w.calories_kcal));
        Observation {
            delta,
            trip,
            closed_trip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trip_idle_boundary_resume_and_unknown_measurements() {
        let c = Config {
            trip_idle_seconds: 300,
            ..Default::default()
        };
        let mut tracker = Tracker::default();
        let mut w = Workout::default();
        w.connection("ready");
        w.stale = false;
        w.current.cadence_rpm = Some(60.);
        w.active_s = 1.;
        w.distance_m = Some(5.);
        let a = tracker.observe(&w, &c, 1000, 10000, false);
        let first = a.trip.unwrap().id;
        assert_eq!(a.delta.distance_m, Some(5.));
        assert_eq!(a.delta.sessions_started, 1);
        assert_eq!(a.delta.trips_started, 1);
        assert_eq!(a.delta.heart_rate_observed_s, 0.);
        w.current.cadence_rpm = Some(0.);
        let a = tracker.observe(&w, &c, 300999, 309999, false);
        assert_eq!(a.trip.unwrap().id, first);
        assert!(a.closed_trip.is_none());
        let a = tracker.observe(&w, &c, 301000, 310000, false);
        assert!(a.trip.is_none());
        let closed = a.closed_trip.unwrap();
        assert_eq!(closed.ended_ms, Some(10000));
        assert_eq!(closed.active_s, 1.);
        assert_eq!(closed.elapsed_s, 1.);
        assert_eq!(closed.calories_kcal, None);
        w.current.cadence_rpm = Some(70.);
        w.active_s = 2.;
        w.distance_m = Some(12.);
        let a = tracker.observe(&w, &c, 302000, 311000, false);
        assert_ne!(a.trip.as_ref().unwrap().id, first);
        assert_eq!(a.trip.unwrap().distance_m, Some(7.));
        let a = tracker.observe(&w, &c, 303000, 312000, true);
        assert_eq!(
            a.closed_trip.unwrap().end_reason.as_deref(),
            Some("collector_stopped")
        );
    }
    #[test]
    fn disconnect_and_pause_do_not_start_trips() {
        let mut tracker = Tracker::default();
        let mut w = Workout::default();
        w.current.speed_kmh = Some(20.);
        assert!(tracker
            .observe(&w, &Config::default(), 1000, 1000, false)
            .trip
            .is_none());
        w.connection("ready");
        w.current.speed_kmh = Some(20.);
        w.stale = false;
        w.paused = true;
        assert!(tracker
            .observe(&w, &Config::default(), 2000, 2000, false)
            .trip
            .is_none());
    }
}
