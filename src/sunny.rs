//! Sunny proprietary framing, established from live SF-B023005DGY captures and
//! Android 2.19.0 protocol routines. No decompiled application code is included.
use crate::{metrics::Sample, storage::hex};
use anyhow::{bail, Result};
use serde_json::json;
pub const COMMAND: &str = "fd710002-e950-458e-8a4d-a1cbc5aa4cce";
pub const RESPONSE: &str = "fd710003-e950-458e-8a4d-a1cbc5aa4cce";
pub const STATUS: &str = "fd710004-e950-458e-8a4d-a1cbc5aa4cce";
pub const STATUS_ACK: &str = "fd710005-e950-458e-8a4d-a1cbc5aa4cce";
pub const MOTION: &str = "fd710006-e950-458e-8a4d-a1cbc5aa4cce";
pub fn frame(command: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x5a, command, 0, payload.len() as u8];
    bytes.extend(payload);
    let sum = bytes[1..].iter().fold(0u8, |n, b| n.wrapping_add(*b));
    bytes.extend([sum, 0xa5]);
    bytes
}
pub fn startup() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            COMMAND.into(),
            frame(2, &[7, 0xa0, b'1', b'2', b'3', b'4', b'5', b'6']),
        ),
        (COMMAND.into(), frame(1, &[])),
        // A3 01 starts reporting WITHOUT clearing existing device session counters.
        (COMMAND.into(), frame(2, &[2, 0xa3, 1])),
        (STATUS_ACK.into(), frame(4, &[])),
    ]
}
#[derive(Default)]
pub struct Framer {
    buffer: Vec<u8>,
    sources: Vec<u64>,
    last_ms: Option<u64>,
}
impl Framer {
    pub fn pending(&self) -> bool {
        !self.buffer.is_empty()
    }
    pub fn push(&mut self, data: &[u8], ms: u64, event: u64) -> Result<Option<Sample>> {
        if self.last_ms.is_some_and(|t| ms.saturating_sub(t) > 2000) {
            self.buffer.clear();
            self.sources.clear();
        }
        self.last_ms = Some(ms);
        if self.buffer.len() + data.len() > 1024 {
            self.buffer.clear();
            self.sources.clear();
            bail!("Sunny frame exceeds buffer bound");
        }
        self.buffer.extend(data);
        self.sources.push(event);
        let result = self.parse();
        if result.is_err() {
            self.buffer.clear();
            self.sources.clear();
        }
        result
    }
    fn parse(&mut self) -> Result<Option<Sample>> {
        let mut sample = Sample::default();
        let mut completed = false;
        let mut frames = Vec::new();
        while self.buffer.len() >= 4 {
            if self.buffer[0] != 0x5a {
                bail!("Sunny frame missing header (fragment gap)");
            }
            let len = self.buffer[3] as usize + 6;
            if self.buffer.len() < len {
                break;
            }
            let frame: Vec<u8> = self.buffer.drain(..len).collect();
            if frame[len - 1] != 0xa5 {
                bail!("Sunny frame missing trailer");
            }
            let sum = frame[1..len - 2]
                .iter()
                .fold(0u8, |s, v| s.wrapping_add(*v));
            if sum != frame[len - 2] {
                bail!("Sunny checksum mismatch");
            }
            let command = frame[1];
            let mut fields = Vec::new();
            let mut pos = 4;
            while pos < len - 2 {
                let field_len = frame[pos] as usize;
                if field_len < 1 || pos + 1 + field_len > len - 2 {
                    bail!("Sunny invalid TLV length");
                }
                let tag = frame[pos + 1];
                let bytes = &frame[pos + 2..pos + 1 + field_len];
                let value = if bytes.len() <= 8 {
                    Some(
                        bytes
                            .iter()
                            .enumerate()
                            .fold(0u64, |n, (i, b)| n | ((*b as u64) << (i * 8))),
                    )
                } else {
                    None
                };
                fields.push(
                    json!({"tag":format!("{tag:02x}"),"value_hex":hex(bytes),"unsigned_le":value}),
                );
                if command == 5 {
                    if let Some(v) = value {
                        let v = v as f64;
                        match (tag, bytes.len()) {
                            (0x20, 2) => sample.speed_kmh = Some(v / 100.),
                            (0x21, 2) => sample.cadence_rpm = Some(v),
                            (0x24, 4) => sample.calories_kcal = Some(v / 10.),
                            (0x25, 1) if v > 0. => sample.heart_rate_bpm = Some(v),
                            (0x26, 2) => sample.device_elapsed_s = Some(v),
                            (0x2a, 4) => sample.distance_m = Some(v),
                            (0x2b, 4) => sample.odometer_m = Some(v),
                            _ => {}
                        }
                    }
                }
                if command == 4 && tag == 0xf1 && bytes.len() == 1 {
                    sample.device_status = Some(
                        match bytes[0] {
                            0 => "initializing",
                            1 => "test",
                            2 => "idle",
                            3 => "ready",
                            4 => "started",
                            5 => "paused",
                            6 => "stopping",
                            7 => "stopped",
                            8 => "fault",
                            9 => "safety",
                            10 => "relaxed",
                            11 => "program_complete",
                            12 => "button",
                            _ => "unknown",
                        }
                        .into(),
                    );
                }
                pos += field_len + 1;
            }
            frames.push(json!({"command":command,"answer_flag":frame[2],"frame_hex":hex(&frame),"fields":fields}));
            completed = true;
        }
        if !completed {
            return Ok(None);
        }
        sample.source_events = self.sources.clone();
        if self.buffer.is_empty() {
            self.sources.clear();
        } else {
            self.sources = self.sources.last().copied().into_iter().collect();
        }
        sample.extra.insert("sunny_frames".into(), json!(frames));
        sample.provenance.insert(
            "protocol".into(),
            "Sunny TLV; Android 2.19.0 mapping and SF-B023005DGY live capture".into(),
        );
        sample.provenance.insert(
            "calories_kcal".into(),
            "device estimate; not a measured energy expenditure".into(),
        );
        Ok(Some(sample))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unhex;
    #[test]
    fn captured_motion_all_fragment_boundaries() {
        let bytes =
            unhex("5a0500210321520003208d08052a78000000052bfd4600000524160000000326140002250011a5")
                .unwrap();
        for split in 1..bytes.len() {
            let mut f = Framer::default();
            assert!(f.push(&bytes[..split], 0, 10).unwrap().is_none());
            let s = f.push(&bytes[split..], 1, 11).unwrap().unwrap();
            assert_eq!(s.speed_kmh, Some(21.89));
            assert_eq!(s.cadence_rpm, Some(82.));
            assert_eq!(s.distance_m, Some(120.));
            assert_eq!(s.odometer_m, Some(18173.));
            assert_eq!(s.calories_kcal, Some(2.2));
            assert_eq!(s.device_elapsed_s, Some(20.));
            assert_eq!(s.heart_rate_bpm, None);
            assert_eq!(s.source_events, vec![10, 11]);
        }
    }
    #[test]
    fn bad_checksum_tlv_and_gap() {
        let mut f = Framer::default();
        let mut b = frame(5, &[3, 0x20, 0, 0]);
        b[6] ^= 1;
        assert!(f.push(&b, 0, 1).is_err());
        assert!(f.push(&frame(5, &[5, 0x20, 0]), 1, 2).is_err());
        let b = frame(4, &[2, 0xf1, 2]);
        assert!(f.push(&b[..4], 2, 3).unwrap().is_none());
        assert!(f.push(&b[4..], 3000, 4).is_err());
        assert_eq!(
            f.push(&b, 3001, 5)
                .unwrap()
                .unwrap()
                .device_status
                .as_deref(),
            Some("idle")
        );
    }
    #[test]
    fn startup_keeps_counters() {
        assert_eq!(hex(&startup()[2].1), "5a02000302a301aba5");
    }
    #[test]
    fn concatenated_frames_are_retained() {
        let mut b = frame(4, &[2, 0xf1, 2]);
        b.extend(frame(4, &[2, 0xf1, 4]));
        let sample = Framer::default().push(&b, 0, 1).unwrap().unwrap();
        assert_eq!(sample.extra["sunny_frames"].as_array().unwrap().len(), 2);
        assert_eq!(sample.device_status.as_deref(), Some("started"));
    }
    #[test]
    fn unknown_tlv_retained() {
        let s = Framer::default()
            .push(&frame(5, &[3, 0xee, 0xab, 0xcd]), 0, 1)
            .unwrap()
            .unwrap();
        assert_eq!(s.extra["sunny_frames"][0]["fields"][0]["value_hex"], "abcd");
    }
}
