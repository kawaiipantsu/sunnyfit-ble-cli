use crate::config::Config;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub speed_kmh: Option<f64>,
    pub cadence_rpm: Option<f64>,
    pub odometer_m: Option<f64>,
    pub distance_m: Option<f64>,
    pub calories_kcal: Option<f64>,
    pub power_w: Option<f64>,
    pub heart_rate_bpm: Option<f64>,
    pub resistance: Option<f64>,
    pub device_elapsed_s: Option<f64>,
    pub battery_percent: Option<f64>,
    pub provenance: BTreeMap<String, String>,
    pub device_status: Option<String>,
    pub source_events: Vec<u64>,
    pub extra: BTreeMap<String, serde_json::Value>,
}
struct Bytes<'a>(&'a [u8], usize);
impl Bytes<'_> {
    fn n(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.1 + n;
        if end > self.0.len() {
            bail!("truncated measurement")
        };
        let b = &self.0[self.1..end];
        self.1 = end;
        Ok(b)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.n(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.n(2)?.try_into()?))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.n(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.n(4)?.try_into()?))
    }
}
#[derive(Default)]
pub struct Decoder {
    sunny: crate::sunny::Framer,
    crank: Option<(u16, u16)>,
    wheel: Option<(u32, u16)>,
    last_csc_ms: Option<u64>,
}
impl Decoder {
    pub fn pending(&self) -> bool {
        self.sunny.pending()
    }
    #[cfg(test)]
    pub fn decode(
        &mut self,
        uuid: &str,
        data: &[u8],
        elapsed_ms: u64,
        c: &Config,
    ) -> Result<Option<Sample>> {
        self.decode_event(uuid, data, elapsed_ms, c, 0)
    }
    pub fn decode_event(
        &mut self,
        uuid: &str,
        data: &[u8],
        elapsed_ms: u64,
        c: &Config,
        event: u64,
    ) -> Result<Option<Sample>> {
        if [
            crate::sunny::MOTION,
            crate::sunny::STATUS,
            crate::sunny::RESPONSE,
        ]
        .contains(&uuid.to_ascii_lowercase().as_str())
        {
            return self.sunny.push(data, elapsed_ms, event);
        }
        let mut b = Bytes(data, 0);
        let mut s = Sample::default();
        match uuid.to_ascii_lowercase().as_str() {
            "00002ad2-0000-1000-8000-00805f9b34fb" => {
                let f = b.u16()?;
                if f & 1 == 0 {
                    s.speed_kmh = Some(b.u16()? as f64 / 100.);
                }
                if f & 2 != 0 {
                    b.n(2)?;
                }
                if f & 4 != 0 {
                    s.cadence_rpm = Some(b.u16()? as f64 / 2.);
                }
                if f & 8 != 0 {
                    b.n(2)?;
                }
                if f & 16 != 0 {
                    let v = b.n(3)?;
                    s.distance_m =
                        Some((v[0] as u32 | ((v[1] as u32) << 8) | ((v[2] as u32) << 16)) as f64);
                }
                if f & 32 != 0 {
                    s.resistance = Some(b.i16()? as f64 / 10.);
                }
                if f & 64 != 0 {
                    s.power_w = Some(b.i16()? as f64);
                }
                if f & 128 != 0 {
                    b.n(2)?;
                }
                if f & 256 != 0 {
                    let v = b.u16()?;
                    if v != u16::MAX {
                        s.calories_kcal = Some(v as f64);
                    }
                    b.n(3)?;
                }
                if f & 512 != 0 {
                    let v = b.u8()?;
                    if v != 255 {
                        s.heart_rate_bpm = Some(v as f64);
                    }
                }
                if f & 1024 != 0 {
                    b.n(1)?;
                }
                if f & 2048 != 0 {
                    s.device_elapsed_s = Some(b.u16()? as f64);
                }
                if f & 4096 != 0 {
                    b.n(2)?;
                }
                s.provenance
                    .insert("protocol".into(), "Bluetooth FTMS; device reported".into());
            }
            "00002a5b-0000-1000-8000-00805f9b34fb" => {
                let f = b.u8()?;
                if f & !3 != 0 || f == 0 {
                    bail!("invalid CSC flags");
                }
                // Parse the entire record before changing counter baselines.
                let wheel = if f & 1 != 0 {
                    Some((b.u32()?, b.u16()?))
                } else {
                    None
                };
                let crank = if f & 2 != 0 {
                    Some((b.u16()?, b.u16()?))
                } else {
                    None
                };
                let continuous = self
                    .last_csc_ms
                    .is_some_and(|t| elapsed_ms.saturating_sub(t) < 64000);
                if let Some((r, t)) = wheel {
                    if continuous {
                        if let (Some((pr, pt)), Some(m)) = (self.wheel, c.wheel_circumference_m) {
                            let dt = t.wrapping_sub(pt) as f64 / 1024.;
                            if let Some(dr) = r.checked_sub(pr) {
                                if dt > 0. {
                                    let speed = dr as f64 * m / dt * 3.6;
                                    if speed <= 150. {
                                        s.speed_kmh = Some(speed);
                                    }
                                }
                            }
                        }
                    }
                    self.wheel = Some((r, t));
                }
                if let Some((r, t)) = crank {
                    if continuous {
                        if let Some((pr, pt)) = self.crank {
                            let dt = t.wrapping_sub(pt) as f64 / 1024.;
                            let dr = r.wrapping_sub(pr);
                            if dt > 0. {
                                let rpm = dr as f64 * 60. / dt;
                                if rpm <= 300. {
                                    s.cadence_rpm = Some(rpm);
                                }
                            }
                        }
                    }
                    self.crank = Some((r, t));
                }
                self.last_csc_ms = Some(elapsed_ms);
                s.provenance.insert(
                    "protocol".into(),
                    "Bluetooth CSC; counter deltas (1024 Hz)".into(),
                );
            }
            "00002a19-0000-1000-8000-00805f9b34fb" => {
                let v = b.u8()?;
                if v <= 100 {
                    s.battery_percent = Some(v as f64);
                }
                s.provenance
                    .insert("battery_percent".into(), "device reported".into());
            }
            _ => return Ok(None),
        }
        Ok(Some(s))
    }
}
#[derive(Clone, Serialize)]
pub struct Workout {
    pub elapsed_s: f64,
    pub active_s: f64,
    pub paused: bool,
    pub distance_m: Option<f64>,
    pub calories_kcal: Option<f64>,
    pub max_speed_kmh: Option<f64>,
    pub average_speed_kmh: Option<f64>,
    pub current: Sample,
    pub stale: bool,
    pub connection: String,
    pub sample_count: u64,
    pub source_event: Option<u64>,
    pub distance_basis: String,
    pub calorie_basis: String,
    #[serde(skip)]
    last_ms: Option<u64>,
    #[serde(skip)]
    field_times: BTreeMap<String, u64>,
    #[serde(skip)]
    distance_baseline: Option<f64>,
    #[serde(skip)]
    calorie_baseline: Option<f64>,
    #[serde(skip)]
    speed_integral: f64,
    #[serde(skip)]
    speed_seconds: f64,
}
impl Default for Workout {
    fn default() -> Self {
        Self {
            elapsed_s: 0.,
            active_s: 0.,
            paused: false,
            distance_m: None,
            calories_kcal: None,
            max_speed_kmh: None,
            average_speed_kmh: None,
            current: Sample::default(),
            stale: true,
            connection: "starting".into(),
            sample_count: 0,
            source_event: None,
            distance_basis: "unavailable".into(),
            calorie_basis: "unavailable".into(),
            last_ms: None,
            field_times: BTreeMap::new(),
            distance_baseline: None,
            calorie_baseline: None,
            speed_integral: 0.,
            speed_seconds: 0.,
        }
    }
}
impl Workout {
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused != paused {
            // A counter interval crossing either pause boundary is not observed
            // entirely within the active workout.
            self.distance_baseline = None;
            self.calorie_baseline = None;
        }
        self.paused = paused;
    }
    pub fn connection(&mut self, state: &str) {
        self.connection = state.into();
        if state != "ready" {
            self.field_times.clear();
            self.current = Sample::default();
            self.stale = true;
            self.distance_baseline = None;
            self.calorie_baseline = None;
        }
    }
    pub fn tick(&mut self, now: u64, c: &Config) {
        let dt = self
            .last_ms
            .map(|p| now.saturating_sub(p) as f64 / 1000.)
            .unwrap_or(0.)
            .min(c.stale_seconds as f64);
        self.last_ms = Some(now);
        self.elapsed_s = now as f64 / 1000.;
        macro_rules! expire {($($field:ident),*)=>{$(if self.field_times.get(stringify!($field)).is_none_or(|t|now.saturating_sub(*t)>c.stale_seconds*1000){self.current.$field=None;})*}}
        expire!(speed_kmh, cadence_rpm, power_w, heart_rate_bpm, resistance);
        self.stale = self.current.speed_kmh.is_none() && self.current.cadence_rpm.is_none();
        let moving = self.current.speed_kmh.is_some_and(|v| v > 0.)
            || self.current.cadence_rpm.is_some_and(|v| v > 0.);
        if self.paused || self.connection != "ready" || !moving {
            return;
        }
        self.active_s += dt;
        if let Some(speed) = self.current.speed_kmh {
            self.speed_integral += speed * dt;
            self.speed_seconds += dt;
            self.average_speed_kmh = Some(self.speed_integral / self.speed_seconds);
            self.max_speed_kmh = Some(self.max_speed_kmh.unwrap_or(0.).max(speed));
            if self.distance_baseline.is_none() {
                *self.distance_m.get_or_insert(0.) += speed / 3.6 * dt;
                self.distance_basis =
                    "integrated speed; excludes stale/disconnected intervals".into();
            }
        }
        if self.calorie_baseline.is_none() {
            if let Some(met) = c.met {
                *self.calories_kcal.get_or_insert(0.) += met * 3.5 * c.weight_kg / 200. / 60. * dt;
                self.calorie_basis = "estimated from user MET and weight".into();
            }
        }
    }
    pub fn ingest(&mut self, mut s: Sample, now: u64, event: u64, c: &Config) {
        self.tick(now, c);
        if self
            .field_times
            .get("distance_m")
            .is_some_and(|t| now.saturating_sub(*t) > c.stale_seconds.saturating_mul(1000))
        {
            self.distance_baseline = None;
        }
        if self
            .field_times
            .get("calories_kcal")
            .is_some_and(|t| now.saturating_sub(*t) > c.stale_seconds.saturating_mul(1000))
        {
            self.calorie_baseline = None;
        }
        self.sample_count += 1;
        self.source_event = Some(event);
        if s.speed_kmh.is_none() {
            if let (Some(rpm), Some(m)) = (s.cadence_rpm, c.meters_per_crank) {
                s.speed_kmh = Some(rpm * m * 0.06);
                s.provenance.insert(
                    "speed_kmh".into(),
                    "derived from user meters_per_crank calibration".into(),
                );
            }
        }
        for (value, baseline, total, basis) in [
            (
                s.distance_m,
                &mut self.distance_baseline,
                &mut self.distance_m,
                &mut self.distance_basis,
            ),
            (
                s.calories_kcal,
                &mut self.calorie_baseline,
                &mut self.calories_kcal,
                &mut self.calorie_basis,
            ),
        ] {
            if let Some(v) = value {
                if !self.paused {
                    if let Some(prev) = *baseline {
                        if v >= prev {
                            *total.get_or_insert(0.) += v - prev;
                        }
                    } else {
                        total.get_or_insert(0.);
                    }
                }
                *baseline = Some(v);
                *basis = "device counter delta; reconnect/reset starts a new baseline".into();
            }
        }
        macro_rules! merge {($($field:ident),*)=>{$(if s.$field.is_some(){self.current.$field=s.$field;self.field_times.insert(stringify!($field).into(),now);})*}}
        merge!(
            speed_kmh,
            cadence_rpm,
            distance_m,
            odometer_m,
            calories_kcal,
            power_w,
            heart_rate_bpm,
            resistance,
            device_elapsed_s,
            battery_percent
        );
        self.current.provenance.extend(s.provenance);
        self.current.extra.extend(s.extra);
        self.current.source_events = s.source_events;
        if s.device_status.is_some() {
            self.current.device_status = s.device_status;
        }
        self.stale = self.current.speed_kmh.is_none() && self.current.cadence_rpm.is_none();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const FTMS: &str = "00002ad2-0000-1000-8000-00805f9b34fb";
    const CSC: &str = "00002a5b-0000-1000-8000-00805f9b34fb";
    #[test]
    fn ftms_optional_fields_and_truncation() {
        let c = Config::default();
        let mut d = Decoder::default();
        let bytes = [
            0x54, 0x01, 0xd2, 0x04, 120, 0, 0xe8, 3, 0, 80, 0, 15, 0, 0, 0, 0,
        ];
        let s = d.decode(FTMS, &bytes, 0, &c).unwrap().unwrap();
        assert_eq!(s.speed_kmh, Some(12.34));
        assert_eq!(s.cadence_rpm, Some(60.));
        assert_eq!(s.distance_m, Some(1000.));
        assert_eq!(s.power_w, Some(80.));
        assert_eq!(s.calories_kcal, Some(15.));
        for end in 0..bytes.len() {
            assert!(d.decode(FTMS, &bytes[..end], 0, &c).is_err());
        }
        assert!(d
            .decode(FTMS, &[1, 0], 0, &c)
            .unwrap()
            .unwrap()
            .speed_kmh
            .is_none());
    }
    #[test]
    fn csc_wrap_duplicate_reset_and_long_gap() {
        let c = Config::default();
        let mut d = Decoder::default();
        assert!(d
            .decode(CSC, &[2, 255, 255, 0, 252], 0, &c)
            .unwrap()
            .unwrap()
            .cadence_rpm
            .is_none());
        assert_eq!(
            d.decode(CSC, &[2, 0, 0, 0, 0], 1000, &c)
                .unwrap()
                .unwrap()
                .cadence_rpm,
            Some(60.)
        );
        assert!(d
            .decode(CSC, &[2, 0, 0, 0, 0], 2000, &c)
            .unwrap()
            .unwrap()
            .cadence_rpm
            .is_none());
        assert!(d
            .decode(CSC, &[2, 100, 0, 0, 4], 3000, &c)
            .unwrap()
            .unwrap()
            .cadence_rpm
            .is_none());
        assert!(d
            .decode(CSC, &[2, 101, 0, 0, 8], 70000, &c)
            .unwrap()
            .unwrap()
            .cadence_rpm
            .is_none());
    }
    #[test]
    fn stale_disconnect_pause_and_counter_reset() {
        let c = Config::default();
        let mut w = Workout::default();
        w.connection("ready");
        w.ingest(
            Sample {
                speed_kmh: Some(3.6),
                distance_m: Some(100.),
                ..Default::default()
            },
            0,
            1,
            &c,
        );
        w.ingest(
            Sample {
                distance_m: Some(110.),
                ..Default::default()
            },
            1000,
            2,
            &c,
        );
        assert_eq!(w.distance_m, Some(10.));
        w.ingest(
            Sample {
                distance_m: Some(0.),
                ..Default::default()
            },
            2000,
            3,
            &c,
        );
        assert_eq!(w.distance_m, Some(10.));
        w.tick(10000, &c);
        assert!(w.stale);
        assert_eq!(w.current.speed_kmh, None);
        w.connection("disconnected");
        w.tick(11000, &c);
        assert_eq!(w.distance_m, Some(10.));
        w.connection("ready");
        w.ingest(
            Sample {
                distance_m: Some(500.),
                ..Default::default()
            },
            12000,
            4,
            &c,
        );
        assert_eq!(w.distance_m, Some(10.));
        w.paused = true;
        w.ingest(
            Sample {
                distance_m: Some(600.),
                ..Default::default()
            },
            13000,
            5,
            &c,
        );
        assert_eq!(w.distance_m, Some(10.));
    }
    #[test]
    fn counter_intervals_exclude_silence_and_pause_boundaries() {
        let c = Config::default();
        let mut w = Workout::default();
        w.connection("ready");
        let sample = |v| Sample {
            distance_m: Some(v),
            calories_kcal: Some(v / 10.),
            ..Default::default()
        };
        w.ingest(sample(100.), 0, 1, &c);
        w.ingest(sample(110.), 1000, 2, &c);
        w.ingest(sample(500.), 10000, 3, &c);
        assert_eq!(w.distance_m, Some(10.));
        assert_eq!(w.calories_kcal, Some(1.));
        w.set_paused(true);
        w.set_paused(false);
        w.ingest(sample(600.), 11000, 4, &c);
        assert_eq!(w.distance_m, Some(10.));
        w.ingest(sample(610.), 12000, 5, &c);
        assert_eq!(w.distance_m, Some(20.));
        assert_eq!(w.calories_kcal, Some(2.));
    }
}
