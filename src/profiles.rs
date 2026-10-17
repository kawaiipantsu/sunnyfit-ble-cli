//! Read-only, declarative decoders. Application writes remain explicit or model guarded.
use crate::{
    config::Config,
    metrics::{Decoder, Sample},
    storage::unhex,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs, path::Path};

pub const HEART_RATE: &str = "00002a37-0000-1000-8000-00805f9b34fb";
pub const BUILTINS: &[&str] = &["auto", "sunny-mini", "ftms", "csc", "heart-rate", "raw"];
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub fields: Vec<Field>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub characteristic: String,
    pub metric: String,
    pub offset: usize,
    pub width: usize,
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub big_endian: bool,
    #[serde(default = "one")]
    pub scale: f64,
}
fn one() -> f64 {
    1.
}
pub fn load(c: &Config) -> Result<Option<Profile>> {
    if let Some(path) = &c.profile_file {
        let p: Profile = toml::from_str(&fs::read_to_string(path)?)
            .map_err(|_| anyhow::anyhow!("invalid device profile"))?;
        if p.id.is_empty() || p.name.is_empty() || p.fields.is_empty() {
            bail!("profile needs id, name and fields");
        }
        for f in &p.fields {
            f.characteristic
                .parse::<uuid::Uuid>()
                .context("profile characteristic must be a UUID")?;
            if ![1, 2, 4].contains(&f.width)
                || f.offset > 512
                || !f.scale.is_finite()
                || f.scale <= 0.
                || ![
                    "speed_kmh",
                    "cadence_rpm",
                    "distance_m",
                    "calories_kcal",
                    "power_w",
                    "heart_rate_bpm",
                    "resistance",
                    "device_elapsed_s",
                    "battery_percent",
                ]
                .contains(&f.metric.as_str())
            {
                bail!("unsupported profile field, width, offset or scale");
            }
        }
        return Ok(Some(p));
    }
    if !BUILTINS.contains(&c.profile.as_str()) {
        bail!("unknown profile; use profiles or --profile-file");
    }
    Ok(None)
}
pub fn decode(p: &Profile, uuid: &str, bytes: &[u8]) -> Result<Option<Sample>> {
    let mut s = serde_json::to_value(Sample::default())?;
    let mut found = false;
    for f in &p.fields {
        if !f.characteristic.eq_ignore_ascii_case(uuid) {
            continue;
        }
        found = true;
        let b = bytes
            .get(f.offset..f.offset + f.width)
            .context("truncated custom profile measurement")?;
        let mut value = 0u64;
        for i in 0..f.width {
            value |= (b[if f.big_endian { f.width - 1 - i } else { i }] as u64) << (8 * i);
        }
        let number = if f.signed {
            ((value << (64 - 8 * f.width)) as i64 >> (64 - 8 * f.width)) as f64
        } else {
            value as f64
        } * f.scale;
        if !number.is_finite() {
            bail!("nonfinite custom profile measurement");
        }
        s[&f.metric] = json!(number);
    }
    if !found {
        return Ok(None);
    }
    s["provenance"]["protocol"] = json!(format!("custom profile: {}", p.id));
    Ok(Some(serde_json::from_value(s)?))
}
pub fn list() {
    println!("auto         Detect Sunny mini bike and standard FTMS/CSC/HRS measurements\nsunny-mini   Sunny SF-B023005DGY (model-guarded startup)\nftms         Bluetooth Fitness Machine / Indoor Bike Data\ncsc          Bluetooth Cycling Speed and Cadence\nheart-rate   Bluetooth HRS: compatible Polar, Garmin, Magene, iGPSport sensors\nraw          Inspect and record without decoding\nCustom       --profile-file profile.toml (read-only field mappings)");
}
pub fn validate(c: &Config, path: &Path) -> Result<usize> {
    let cases: Vec<Value> = serde_json::from_slice(&fs::read(path)?)?;
    let mut count = 0;
    for case in cases {
        let mut d = Decoder::default();
        for frame in case["frames"].as_array().context("fixture needs frames")? {
            let result = d.decode_event(
                frame["uuid"].as_str().context("fixture UUID")?,
                &unhex(frame["hex"].as_str().context("fixture hex")?)?,
                frame["elapsed_ms"].as_u64().unwrap_or(0),
                c,
                0,
            );
            if frame["error"] == true {
                if result.is_ok() {
                    bail!("fixture {} expected rejection", case["name"]);
                }
            } else {
                let actual = serde_json::to_value(result?)?;
                if let Some(expected) = frame["expected"].as_object() {
                    for (k, v) in expected {
                        if actual[k] != *v {
                            bail!("fixture {} field {} mismatch", case["name"], k);
                        }
                    }
                }
            }
            count += 1;
        }
    }
    Ok(count)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_endianness_signed_and_bounds() {
        let p = Profile {
            id: "example".into(),
            name: "Example".into(),
            fields: vec![Field {
                characteristic: HEART_RATE.into(),
                metric: "power_w".into(),
                offset: 1,
                width: 2,
                signed: true,
                big_endian: true,
                scale: 0.5,
            }],
        };
        assert_eq!(
            decode(&p, HEART_RATE, &[0, 255, 254])
                .unwrap()
                .unwrap()
                .power_w,
            Some(-1.)
        );
        assert!(decode(&p, HEART_RATE, &[0, 1]).is_err());
    }
    #[test]
    fn hrs_fixtures() {
        assert!(validate(&Config::default(), Path::new("fixtures/heart-rate.json")).unwrap() >= 8);
    }
}
