use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub adapter: Option<String>,
    pub device: Option<String>,
    pub pair: bool,
    pub reconnect: bool,
    pub timeout_seconds: u64,
    pub reconnect_max_seconds: u64,
    pub stale_seconds: u64,
    pub poll_seconds: u64,
    pub init_command: Vec<String>,
    pub sunny_protocol: bool,
    pub wheel_circumference_m: Option<f64>,
    pub meters_per_crank: Option<f64>,
    pub weight_kg: f64,
    pub met: Option<f64>,
    pub goal_minutes: u64,
    pub target_cadence: f64,
    pub redis_enabled: bool,
    pub redis_url: Option<String>,
    pub raw_channel: String,
    pub metrics_channel: String,
    pub status_channel: String,
    pub workout_channel: String,
    pub redis_queue: usize,
    pub output_dir: Option<PathBuf>,
    pub debug_stdout: bool,
    pub verbose: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            adapter: None,
            device: None,
            pair: false,
            reconnect: true,
            timeout_seconds: 20,
            reconnect_max_seconds: 30,
            stale_seconds: 5,
            poll_seconds: 0,
            init_command: Vec::new(),
            sunny_protocol: true,
            wheel_circumference_m: None,
            meters_per_crank: None,
            weight_kg: 75.,
            met: None,
            goal_minutes: 20,
            target_cadence: 60.,
            redis_enabled: true,
            redis_url: None,
            raw_channel: "sunny:bike:raw".into(),
            metrics_channel: "sunny:bike:metrics".into(),
            status_channel: "sunny:bike:status".into(),
            workout_channel: "sunny:bike:workout".into(),
            redis_queue: 512,
            output_dir: None,
            debug_stdout: false,
            verbose: false,
        }
    }
}
#[derive(Parser)]
#[command(version, about = "SunnyFit BLE recorder, workout TUI and Redis bridge")]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// Show the live workout dashboard (on stderr; stdout stays machine-readable).
    #[arg(long)]
    pub workout: bool,
    /// Use simulated FTMS data; never connects to Bluetooth.
    #[arg(long, global = true)]
    pub demo: bool,
    /// Stop after this many seconds, including connection/reconnect time.
    #[arg(long, global=true, value_parser=clap::value_parser!(u64).range(1..))]
    pub seconds: Option<u64>,
    #[command(flatten)]
    pub settings: Overrides,
    #[command(subcommand)]
    pub command: Option<Command>,
}
#[derive(Subcommand)]
pub enum Command {
    /// Discover nearby LE devices for --seconds (default 15).
    Scan,
    /// Inventory and read every accessible characteristic and descriptor.
    Inspect { address: Option<bluer::Address> },
    /// Verify a GATT connection and remember the bike (no bond required).
    Connect { address: Option<bluer::Address> },
    /// Bond using a Just Works agent and remember the selected device.
    Pair { address: Option<bluer::Address> },
    /// Remove the selected BlueZ bond/cache and clear the saved selection.
    Forget { address: Option<bluer::Address> },
    /// Disconnect the selected bike.
    Disconnect { address: Option<bluer::Address> },
    /// Record all observable GATT data, with reconnects.
    Capture { address: Option<bluer::Address> },
    /// List saved workout summaries as JSON lines.
    History,
    /// Decode a recorded events.jsonl offline; optionally forward to Redis.
    Replay { path: PathBuf },
    /// Check adapter power, BlueZ, and Realtek firmware without connecting.
    Doctor,
    /// Print the config file location (never its secret values).
    Config,
}
#[derive(Args, Default)]
pub struct Overrides {
    #[arg(long, global = true)]
    pub adapter: Option<String>,
    #[arg(long, global = true)]
    pub device: Option<bluer::Address>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true")]
    pub pair: Option<bool>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true")]
    pub reconnect: Option<bool>,
    #[arg(long, global = true)]
    pub timeout_seconds: Option<u64>,
    #[arg(long, global = true)]
    pub reconnect_max_seconds: Option<u64>,
    #[arg(long, global = true)]
    pub stale_seconds: Option<u64>,
    /// Poll readable characteristics; 0 disables polling (default).
    #[arg(long, global = true)]
    pub poll_seconds: Option<u64>,
    /// Explicit initialization UUID=HEX, repeatable; replayed on each connection.
    #[arg(long, global = true)]
    pub init_command: Vec<String>,
    /// Enable observed Sunny startup only for the exact SF-B023005DGY model.
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true")]
    pub sunny_protocol: Option<bool>,
    #[arg(long, global = true)]
    pub wheel_circumference_m: Option<f64>,
    /// Calibrated virtual distance per crank turn; no assumed bike gearing.
    #[arg(long, global = true)]
    pub meters_per_crank: Option<f64>,
    #[arg(long, global = true)]
    pub weight_kg: Option<f64>,
    /// Explicit MET assumption for estimated calories while moving.
    #[arg(long, global = true)]
    pub met: Option<f64>,
    #[arg(long, global = true)]
    pub goal_minutes: Option<u64>,
    #[arg(long, global = true)]
    pub target_cadence: Option<f64>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true")]
    pub redis_enabled: Option<bool>,
    #[arg(long, global = true, env = "SUNNY_REDIS_URL", hide_env_values = true)]
    pub redis_url: Option<String>,
    #[arg(long, global = true)]
    pub raw_channel: Option<String>,
    #[arg(long, global = true)]
    pub metrics_channel: Option<String>,
    #[arg(long, global = true)]
    pub status_channel: Option<String>,
    #[arg(long, global = true)]
    pub workout_channel: Option<String>,
    #[arg(long, global = true)]
    pub redis_queue: Option<usize>,
    #[arg(long, global = true)]
    pub output_dir: Option<PathBuf>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true")]
    pub debug_stdout: Option<bool>,
    #[arg(long,short='v',global=true,num_args=0..=1,default_missing_value="true")]
    pub verbose: Option<bool>,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).context("reading configuration file")?;
        // TOML errors can contain source lines with passwords; intentionally suppress them.
        toml::from_str(&text).map_err(|_| {
            anyhow::anyhow!("invalid config TOML or unknown setting; check the config file")
        })
    }
    pub fn apply(&mut self, o: &Overrides) -> Result<()> {
        macro_rules! set { ($($f:ident),*) => { $(if let Some(v)=&o.$f { self.$f=v.clone(); })* } }
        macro_rules! opt { ($($f:ident),*) => { $(if let Some(v)=&o.$f { self.$f=Some(v.clone()); })* } }
        set!(
            sunny_protocol,
            pair,
            reconnect,
            timeout_seconds,
            reconnect_max_seconds,
            stale_seconds,
            poll_seconds,
            weight_kg,
            goal_minutes,
            target_cadence,
            redis_enabled,
            raw_channel,
            metrics_channel,
            status_channel,
            workout_channel,
            redis_queue,
            debug_stdout,
            verbose
        );
        opt!(
            adapter,
            wheel_circumference_m,
            meters_per_crank,
            met,
            redis_url,
            output_dir
        );
        if !o.init_command.is_empty() {
            self.init_command = o.init_command.clone();
        }
        for cmd in &self.init_command {
            let (u, h) = cmd
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("init-command requires UUID=HEX"))?;
            u.parse::<uuid::Uuid>()
                .map_err(|_| anyhow::anyhow!("invalid init-command UUID"))?;
            crate::storage::unhex(h)?;
        }
        if let Some(v) = o.device {
            self.device = Some(v.to_string());
        }
        if self.timeout_seconds == 0
            || self.reconnect_max_seconds == 0
            || self.stale_seconds == 0
            || self.goal_minutes == 0
            || self.redis_queue == 0
        {
            bail!("timeouts, goal minutes and queue size must be positive");
        }
        for v in [
            Some(self.weight_kg),
            Some(self.target_cadence),
            self.met,
            self.wheel_circumference_m,
            self.meters_per_crank,
        ]
        .into_iter()
        .flatten()
        {
            if !v.is_finite() || v <= 0. {
                bail!("calibration and workout settings must be finite positive numbers");
            }
        }
        if let Some(v) = &self.device {
            v.parse::<bluer::Address>()
                .map_err(|_| anyhow::anyhow!("invalid configured bike address"))?;
        }
        if [
            &self.raw_channel,
            &self.metrics_channel,
            &self.status_channel,
            &self.workout_channel,
        ]
        .iter()
        .any(|s| s.is_empty())
        {
            bail!("Redis channel names cannot be empty");
        }
        Ok(())
    }
}
pub fn private_dir(path: &Path) -> Result<()> {
    let existed = path.exists();
    fs::create_dir_all(path)?;
    if !existed {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    f.write_all(contents)?;
    f.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}
pub fn init() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("cannot find home configuration directory")?;
    let dir = base.config_dir().join("sunnyfit-ble-cli");
    private_dir(&dir)?;
    private_dir(&dir.join("past-workouts"))?;
    let path = dir.join("config.toml");
    if !path.exists() {
        let content=format!("# SunnyFit: command-line settings override this file.\n# Optional: adapter = \"hci1\", device = \"AA:BB:CC:DD:EE:FF\"\n# Optional: meters_per_crank = 1.0, met = 3.0 (user calibration/assumption)\n# Prefer SUNNY_REDIS_URL for secrets. Unknown measurements remain null.\n{}",toml::to_string_pretty(&Config::default())?);
        use std::io::Write;
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(content.as_bytes())?;
                f.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    #[test]
    fn cli_shape_is_valid() {
        Cli::command().debug_assert();
    }
    #[test]
    fn individual_overrides_preserve_other_settings() {
        let mut c = Config {
            goal_minutes: 42,
            met: Some(3.0),
            ..Default::default()
        };
        let cli = Cli::try_parse_from([
            "sunnyfit-ble-cli",
            "--goal-minutes",
            "10",
            "--debug-stdout",
            "--reconnect=false",
            "--sunny-protocol=false",
        ])
        .unwrap();
        c.apply(&cli.settings).unwrap();
        assert_eq!(c.goal_minutes, 10);
        assert_eq!(c.met, Some(3.));
        assert!(c.debug_stdout);
        assert!(!c.reconnect);
        assert!(!c.sunny_protocol);
    }
    #[test]
    fn invalid_numeric_config_rejected() {
        let mut c = Config {
            met: Some(f64::NAN),
            ..Default::default()
        };
        assert!(c.apply(&Overrides::default()).is_err());
        let mut c = Config {
            timeout_seconds: 0,
            ..Default::default()
        };
        assert!(c.apply(&Overrides::default()).is_err());
    }
}
