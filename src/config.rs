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
    pub systemd: bool,
    pub stream_socket: PathBuf,
    pub workout_source: String,
    pub trip_idle_seconds: u64,
    pub sinks: Vec<String>,
    pub mysql_url: Option<String>,
    pub sqlite_path: PathBuf,
    pub database_queue: usize,
    pub log_file: PathBuf,
    pub profile: String,
    pub profile_file: Option<PathBuf>,
    #[serde(skip)]
    pub loaded_profile: Option<crate::profiles::Profile>,
    pub heart_rate_device: Option<String>,
    pub heart_rate_adapter: Option<String>,
    pub food_file: Option<PathBuf>,
    pub interval_plan: Option<PathBuf>,
    pub ghost: Option<String>,
    pub route: String,
    pub daily_goal_minutes: u64,
    pub focus_minutes: u64,
    pub break_minutes: u64,
    pub movement_reminder_minutes: u64,
    pub cadence_alert_rpm: f64,
    pub cadence_alert_seconds: u64,
    pub notifications: bool,

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
            systemd: false,
            stream_socket: PathBuf::from(crate::live::DEFAULT_SOCKET),
            workout_source: "auto".into(),
            trip_idle_seconds: 300,
            sinks: vec!["local".into(), "redis".into()],
            mysql_url: None,
            sqlite_path: PathBuf::from("/var/lib/sunnyfit-ble-cli/workouts.sqlite3"),
            database_queue: 2048,
            log_file: PathBuf::from("/var/log/sunnyfit-ble-cli.log"),
            profile: "auto".into(),
            profile_file: None,
            loaded_profile: None,
            heart_rate_device: None,
            heart_rate_adapter: None,
            food_file: None,
            interval_plan: None,
            ghost: None,
            route: "denmark".into(),
            daily_goal_minutes: 30,
            focus_minutes: 25,
            break_minutes: 5,
            movement_reminder_minutes: 30,
            cadence_alert_rpm: 30.,
            cadence_alert_seconds: 20,
            notifications: false,
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
    #[arg(long, global = true, env = "SUNNY_CONFIG")]
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
    /// Guided discovery and GATT inspection, then remember a compatible bike.
    Wizard { address: Option<bluer::Address> },
    /// List built-in and optional declarative device profiles.
    Profiles,
    /// Validate a captured-frame fixture against the selected profile.
    ValidateProfile { fixture: PathBuf },
    /// Export indoor cycling telemetry to CSV, TCX, or FIT.
    Export {
        input: Option<PathBuf>,
        #[arg(long, value_parser=["csv","tcx","fit"])]
        format: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, value_parser=["sqlite","mysql"], default_value="sqlite")]
        database: String,
    },
    /// List sessions stored in SQLite or MySQL.
    Sessions {
        #[arg(long, value_parser=["sqlite","mysql"], default_value="sqlite")]
        database: String,
    },
    /// Explicitly upload an indoor session to Strava (OAuth credentials from environment/private token file).
    Sync {
        input: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Print a Strava authorization URL, or exchange an authorization code from SUNNY_STRAVA_CODE.
    StravaAuth {
        #[arg(long)]
        exchange: bool,
    },
    /// Delete only the selected statistics table and create the current aggregate schema.
    ResetStatistics {
        #[arg(long, default_value = "sqlite", value_parser = ["mysql", "sqlite"])]
        database: String,
        #[arg(long)]
        confirm_reset: bool,
    },
    /// Print the config file location (never its secret values).
    Config,
}
#[derive(Args, Default)]
pub struct Overrides {
    /// Local read-only service stream socket.
    #[arg(
        long,
        global = true,
        env = "SUNNY_STREAM_SOCKET",
        hide_env_values = true
    )]
    pub stream_socket: Option<PathBuf>,
    /// Workout input: auto attaches to the service, otherwise owns Bluetooth.
    #[arg(long, global=true, env="SUNNY_WORKOUT_SOURCE", value_parser=["auto","service","direct"])]
    pub workout_source: Option<String>,
    /// End an automatic trip after this many idle seconds; 0 disables trip detection.
    #[arg(long, global = true, env = "SUNNY_TRIP_IDLE_SECONDS")]
    pub trip_idle_seconds: Option<u64>,
    /// Headless connector: external sinks and a service log, without workout history/UI.
    #[arg(long, global=true, env="SUNNY_SYSTEMD", num_args=0..=1, default_missing_value="true")]
    pub systemd: Option<bool>,
    /// Comma-separated destinations: local, redis, mysql, sqlite. May enable several.
    #[arg(long, global = true, env = "SUNNY_SINKS", value_delimiter = ',')]
    pub sinks: Option<Vec<String>>,
    #[arg(long, global = true, env = "SUNNY_MYSQL_URL", hide_env_values = true)]
    pub mysql_url: Option<String>,
    #[arg(long, global = true, env = "SUNNY_SQLITE_PATH")]
    pub sqlite_path: Option<PathBuf>,
    #[arg(long, global = true, env = "SUNNY_DATABASE_QUEUE")]
    pub database_queue: Option<usize>,
    #[arg(long, global = true, env = "SUNNY_LOG_FILE")]
    pub log_file: Option<PathBuf>,
    #[arg(long, global = true, env = "SUNNY_PROFILE")]
    pub profile: Option<String>,
    #[arg(long, global = true, env = "SUNNY_PROFILE_FILE")]
    pub profile_file: Option<PathBuf>,
    #[arg(long, global = true, env = "SUNNY_HEART_RATE_DEVICE")]
    pub heart_rate_device: Option<bluer::Address>,
    #[arg(long, global = true, env = "SUNNY_HEART_RATE_ADAPTER")]
    pub heart_rate_adapter: Option<String>,
    #[arg(long, global = true, env = "SUNNY_FOOD_FILE")]
    pub food_file: Option<PathBuf>,
    #[arg(long, global = true, env = "SUNNY_INTERVAL_PLAN")]
    pub interval_plan: Option<PathBuf>,
    /// Previous run directory/events file, or latest/best.
    #[arg(long, global = true, env = "SUNNY_GHOST")]
    pub ghost: Option<String>,
    #[arg(long, global = true, env = "SUNNY_ROUTE")]
    pub route: Option<String>,
    #[arg(long, global = true, env = "SUNNY_DAILY_GOAL_MINUTES")]
    pub daily_goal_minutes: Option<u64>,
    #[arg(long, global = true, env = "SUNNY_FOCUS_MINUTES")]
    pub focus_minutes: Option<u64>,
    #[arg(long, global = true, env = "SUNNY_BREAK_MINUTES")]
    pub break_minutes: Option<u64>,
    #[arg(long, global = true, env = "SUNNY_MOVEMENT_REMINDER_MINUTES")]
    pub movement_reminder_minutes: Option<u64>,
    #[arg(long, global = true, env = "SUNNY_CADENCE_ALERT_RPM")]
    pub cadence_alert_rpm: Option<f64>,
    #[arg(long, global = true, env = "SUNNY_CADENCE_ALERT_SECONDS")]
    pub cadence_alert_seconds: Option<u64>,
    #[arg(long, global=true, env="SUNNY_NOTIFICATIONS", num_args=0..=1, default_missing_value="true")]
    pub notifications: Option<bool>,

    #[arg(long, global = true, env = "SUNNY_ADAPTER", hide_env_values = true)]
    pub adapter: Option<String>,
    #[arg(long, global = true, env = "SUNNY_DEVICE", hide_env_values = true)]
    pub device: Option<bluer::Address>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_PAIR", hide_env_values=true)]
    pub pair: Option<bool>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_RECONNECT", hide_env_values=true)]
    pub reconnect: Option<bool>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_TIMEOUT_SECONDS",
        hide_env_values = true
    )]
    pub timeout_seconds: Option<u64>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_RECONNECT_MAX_SECONDS",
        hide_env_values = true
    )]
    pub reconnect_max_seconds: Option<u64>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_STALE_SECONDS",
        hide_env_values = true
    )]
    pub stale_seconds: Option<u64>,
    /// Poll readable characteristics; 0 disables polling (default).
    #[arg(
        long,
        global = true,
        env = "SUNNY_POLL_SECONDS",
        hide_env_values = true
    )]
    pub poll_seconds: Option<u64>,
    /// Explicit initialization UUID=HEX, repeatable; replayed on each connection.
    #[arg(
        long,
        global = true,
        env = "SUNNY_INIT_COMMAND",
        hide_env_values = true
    )]
    pub init_command: Vec<String>,
    /// Enable observed Sunny startup only for the exact SF-B023005DGY model.
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_SUNNY_PROTOCOL", hide_env_values=true)]
    pub sunny_protocol: Option<bool>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_WHEEL_CIRCUMFERENCE_M",
        hide_env_values = true
    )]
    pub wheel_circumference_m: Option<f64>,
    /// Calibrated virtual distance per crank turn; no assumed bike gearing.
    #[arg(
        long,
        global = true,
        env = "SUNNY_METERS_PER_CRANK",
        hide_env_values = true
    )]
    pub meters_per_crank: Option<f64>,
    #[arg(long, global = true, env = "SUNNY_WEIGHT_KG", hide_env_values = true)]
    pub weight_kg: Option<f64>,
    /// Explicit MET assumption for estimated calories while moving.
    #[arg(long, global = true, env = "SUNNY_MET", hide_env_values = true)]
    pub met: Option<f64>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_GOAL_MINUTES",
        hide_env_values = true
    )]
    pub goal_minutes: Option<u64>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_TARGET_CADENCE",
        hide_env_values = true
    )]
    pub target_cadence: Option<f64>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_REDIS_ENABLED", hide_env_values=true)]
    pub redis_enabled: Option<bool>,
    #[arg(long, global = true, env = "SUNNY_REDIS_URL", hide_env_values = true)]
    pub redis_url: Option<String>,
    #[arg(long, global = true, env = "SUNNY_RAW_CHANNEL", hide_env_values = true)]
    pub raw_channel: Option<String>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_METRICS_CHANNEL",
        hide_env_values = true
    )]
    pub metrics_channel: Option<String>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_STATUS_CHANNEL",
        hide_env_values = true
    )]
    pub status_channel: Option<String>,
    #[arg(
        long,
        global = true,
        env = "SUNNY_WORKOUT_CHANNEL",
        hide_env_values = true
    )]
    pub workout_channel: Option<String>,
    #[arg(long, global = true, env = "SUNNY_REDIS_QUEUE", hide_env_values = true)]
    pub redis_queue: Option<usize>,
    #[arg(long, global = true, env = "SUNNY_OUTPUT_DIR", hide_env_values = true)]
    pub output_dir: Option<PathBuf>,
    #[arg(long,global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_DEBUG_STDOUT", hide_env_values=true)]
    pub debug_stdout: Option<bool>,
    #[arg(long,short='v',global=true,num_args=0..=1,default_missing_value="true", env="SUNNY_VERBOSE", hide_env_values=true)]
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
            systemd,
            stream_socket,
            workout_source,
            trip_idle_seconds,
            sinks,
            sqlite_path,
            database_queue,
            log_file,
            profile,
            route,
            daily_goal_minutes,
            focus_minutes,
            break_minutes,
            movement_reminder_minutes,
            cadence_alert_rpm,
            cadence_alert_seconds,
            notifications,
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
            food_file,
            mysql_url,
            profile_file,
            heart_rate_adapter,
            interval_plan,
            ghost,
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
        if let Some(address) = o.heart_rate_device {
            self.heart_rate_device = Some(address.to_string());
        }
        if !["auto", "service", "direct"].contains(&self.workout_source.as_str()) {
            bail!("workout-source must be auto, service or direct");
        }
        for sink in &self.sinks {
            if !["local", "redis", "mysql", "sqlite"].contains(&sink.as_str()) {
                bail!("sinks must be local, redis, mysql, or sqlite");
            }
        }
        self.sinks.sort();
        self.sinks.dedup();
        if self.systemd {
            self.sinks.retain(|s| s != "local");
            self.debug_stdout = false;
            self.verbose = false;
        }
        if self.sinks.is_empty() {
            bail!("select at least one data sink with --sinks");
        }
        if self.sinks.iter().any(|s| s == "mysql") && self.mysql_url.is_none() {
            bail!("mysql sink requires SUNNY_MYSQL_URL or --mysql-url");
        }
        if self.systemd
            && !self.sinks.iter().any(|s| {
                s == "sqlite"
                    || s == "mysql"
                    || (s == "redis" && self.redis_enabled && self.redis_url.is_some())
            })
        {
            bail!("systemd mode requires an external sink; use --sinks sqlite, mysql, or configured redis");
        }
        if self.trip_idle_seconds > 86400 {
            bail!("trip-idle-seconds must be at most 86400 (or 0 to disable)");
        }
        if self.database_queue == 0
            || self.daily_goal_minutes == 0
            || self.focus_minutes == 0
            || self.break_minutes == 0
            || self.movement_reminder_minutes == 0
            || self.cadence_alert_seconds == 0
        {
            bail!("queue and timer settings must be positive");
        }
        if !self.cadence_alert_rpm.is_finite() || self.cadence_alert_rpm < 0. {
            bail!("cadence alert must be finite and nonnegative");
        }
        if let Some(address) = &self.heart_rate_device {
            address
                .parse::<bluer::Address>()
                .map_err(|_| anyhow::anyhow!("invalid heart-rate device address"))?;
        }
        if !["denmark", "route66", "summit"].contains(&self.route.as_str()) {
            bail!("route must be denmark, route66, or summit");
        }
        self.loaded_profile = crate::profiles::load(self)?;
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
        if self.heart_rate_device.is_some() && self.heart_rate_device == self.device {
            bail!("external heart-rate sensor must differ from the bike");
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
    let service = std::env::args().any(|a| a == "--systemd" || a == "--systemd=true")
        || std::env::var("SUNNY_SYSTEMD").is_ok_and(|v| v == "true");
    if !service {
        private_dir(&dir.join("past-workouts"))?;
    }
    let food = dir.join("callories-burn.json");
    if !food.exists() {
        write_private(&food, include_bytes!("../callories-burn.json"))?;
    }
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
