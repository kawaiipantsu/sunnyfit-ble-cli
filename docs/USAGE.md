# CLI reference

A Rust application for Debian/Linux that connects to a **Sunny Health & Fitness
SF-B023005DGY mini bike**, records its Bluetooth data, runs a live terminal workout,
and publishes JSON events to Redis.

The proprietary startup and telemetry were exercised on a physical bike. Its
Android app's protocol code was inspected to establish field meanings and units.
Speed, cadence, distance, device time, calories, odometer and operating status are
decoded. Calories are the bike's estimate. Power, heart rate, resistance and battery
remain unavailable unless a supported source actually supplies them. See
[protocol and evidence](PROTOCOL.md) and [validation](../VALIDATION.md).

## Build and install

Rust 1.90+ is required. This host was built using Rust 1.94.1 on Debian 13.

```sh
sudo apt-get install build-essential pkg-config libdbus-1-dev bluez firmware-realtek dpkg-dev python3 redis-server redis-tools
sudo systemctl start bluetooth
make                 # list targets
make build
make check           # format, Clippy, unit and isolated integration tests
make deb             # optimized executable and dist/*.deb
sudo apt install ./dist/sunnyfit-ble-cli_0.3.0-1_amd64.deb
```

The Realtek USB adapter already uses the kernel's `btusb`/`btrtl` drivers. The
missing `rtl_bt/rtl8761bu_fw.bin` is supplied by Debian's `firmware-realtek` package
in `non-free-firmware`. Replug that adapter after installation. You do not need
an out-of-tree driver. `make deps` installs prerequisites when run as root.

`make release FEATURES=vendored-dbus` builds the D-Bus client library from source.
A running BlueZ/system D-Bus service is still required. Ordinary operation uses
the host's existing Bluetooth permissions; the program does not change system
D-Bus policies. `make install PREFIX=/usr/local` supports `DESTDIR` staging.

## Connect and ride

Wake the bike and disconnect the phone's SunnyFit connection first.

```sh
sunnyfit-ble-cli doctor
sunnyfit-ble-cli --adapter hci1 --seconds 20 scan
sunnyfit-ble-cli --adapter hci1 connect AA:BB:CC:DD:EE:FF
sunnyfit-ble-cli --workout
```

Use the address found by `scan`. `connect` verifies the GATT connection and saves
only the device/adapter selection. It disconnects its own setup connection when
finished; subsequent captures reconnect automatically. Never select a device by
name alone. Address changes require selecting the bike again.

`pair ADDRESS` explicitly attempts Bluetooth bonding with a Just Works agent.
**The tested mini bike rejected bonding with `AuthenticationFailed`; it streams
successfully over an unbonded GATT connection. Use `connect` for this model.**
For PIN/passkey devices use `bluetoothctl`'s interactive agent.

The built-in startup activates only after reading the exact SF-B023005DGY model
identifier. It authenticates the Sunny application protocol, queries metadata,
starts reporting with the app's non-clearing flag, and acknowledges status.
`--sunny-protocol=false` disables all automatic application writes. It never
sets resistance, sends OTA commands, or resets counters intentionally.

```sh
sunnyfit-ble-cli disconnect
sunnyfit-ble-cli forget             # remove BlueZ bond/cache and saved selection
sunnyfit-ble-cli --debug-stdout -v inspect
sunnyfit-ble-cli --seconds 120 capture
sunnyfit-ble-cli --demo --workout --met 3
```

The workout screen shows speed, cadence, workout distance/energy, wall and active
time, a cadence trace and an active-time goal. `space`/`p` pauses local totals,
`m` records a marker, and `q` saves and exits. Pausing local totals does not control
the bike. Ctrl-C/SIGTERM saves and closes the connection. The TUI uses **stderr**,
so JSON stdout can be redirected separately. `--demo` clearly labels simulated
input and never opens Bluetooth.

## Configuration and history

Every invocation, including `--help`, creates a private default config if absent:

```text
~/.config/sunnyfit-ble-cli/config.toml
~/.config/sunnyfit-ble-cli/past-workouts/<timestamp>-<run-id>/
    manifest.json
    events.jsonl
    summary.json
```

`XDG_CONFIG_HOME` is respected. `--config PATH` loads another config; the default
is still created. Unknown configuration keys and invalid values are rejected.
Each setting has a corresponding command-line option; options override only the
individual setting for that invocation. `connect`, `pair`, and `forget` persist
selection changes, without persisting transient CLI secrets.

Example settings (all optional):

```toml
adapter = "hci1"
device = "AA:BB:CC:DD:EE:FF"
sunny_protocol = true
pair = false
reconnect = true
timeout_seconds = 20
reconnect_max_seconds = 30
stale_seconds = 5
poll_seconds = 0
goal_minutes = 20
target_cadence = 60.0
weight_kg = 75.0
redis_enabled = true
raw_channel = "sunny:bike:raw"
metrics_channel = "sunny:bike:metrics"
status_channel = "sunny:bike:status"
workout_channel = "sunny:bike:workout"
redis_queue = 512
debug_stdout = false
verbose = false
init_command = []
```

Optional `redis_url`, `output_dir`, `wheel_circumference_m`, `meters_per_crank`, and
`met` can also be set in TOML or through their hyphenated CLI options. `met` is an
explicit user assumption for calorie estimation when the device supplies none.
No default gearing or exercise intensity is invented. Standard FTMS and CSC
sources are also decoded; CSC speed needs wheel circumference or a calibrated
metres-per-crank value. The mini bike supplies its own speed/cadence and does not
need calibration for those fields.

Boolean overrides accept `--reconnect=false`, `--redis-enabled=false`, and so on.
`--seconds` bounds the entire operation including setup and reconnects. A silent
but connected device is marked stale after the configured interval; silence
alone does not trigger reconnect or fabricate zero readings.

```sh
sunnyfit-ble-cli history
sunnyfit-ble-cli replay /path/to/events.jsonl
```

Local history contains personal exercise data and BLE identifiers. New capture
directories are mode 0700 and files 0600. Captures, ZIPs, app analysis artifacts,
private configuration, and Debian packages are excluded from Git.

## Output and Redis

| Invocation | Stdout |
| --- | --- |
| Default capture | Quiet |
| `--debug-stdout` | Simple JSON lifecycle/summary messages |
| `-v` alone | Quiet |
| `-v --debug-stdout` | Every raw event and decoded/derived event, one JSON object per line |
| `scan`, `history`, `replay` | Their requested JSON results |

Raw events retain original packet boundaries, bytes, source UUIDs and instance
IDs. Each event has a run ID, sequence, connection epoch where applicable,
wall-clock processing time, and monotonic observed/processed times. Decoded Sunny
frames include the source-event IDs for every fragment, full frame bytes, all
TLVs (including unknown tags), and interpretation provenance.

Prefer the environment or a private config for credential-bearing URLs:

```sh
export SUNNY_REDIS_URL=redis://127.0.0.1:6379/
sunnyfit-ble-cli --workout
redis-cli PSUBSCRIBE 'sunny:bike:*'
```

| Default channel | Contents |
| --- | --- |
| `sunny:bike:raw` | Raw packets, reads, writes, metadata and acquisition outcomes |
| `sunny:bike:metrics` | Decoded samples and periodic workout snapshots |
| `sunny:bike:status` | Connection state, gaps, retries, markers and decoder errors |
| `sunny:bike:workout` | End-of-session workout summary |

Channels are independently configurable. Redis connection attempts back off and
recover. Its bounded queue cannot block BLE collection. Queue drops, failed or
unconfirmed publications and pending messages are saved in the final local
summary. A failed publish is not blindly retried because a timeout can occur
after delivery. `replay` can republish saved data; deduplicate raw events by
`run_id` and `sequence`, decoded events by run and source references. Pub/Sub has
no persistent delivery guarantee. This build supports plain Redis transports;
TLS is not enabled. Do not send credentials over an untrusted plaintext network.

Local capture has a separate bounded writer queue (4096 events), flushes and
`sync_data`s at least once per second while making progress, and syncs on clean
shutdown. Writer failure/overload aborts capture and marks it partial when a
summary can be written. A crash/disk failure may leave an `in_progress` manifest
and lose a buffered tail. Stdout has a separate best-effort queue; its loss count
is in the local summary. Slow stdout and Redis do not stop local recording.

## Acquisition, reconnects and research

A reconnect starts a new epoch, re-discovers the exact selected address, recreates
GATT objects/subscriptions and repeats the verified startup. Counter baselines
reset at a new epoch so missing intervals are not invented as recorded distance.
A device counter decrease also starts a new baseline. Historical odometer and
device counters remain separate from this CLI workout's totals.

All discovered characteristics/descriptors are inventoried and readable values
are attempted. All telemetry notifications are subscribed; `AcquireNotify` is
preferred to avoid the ordinary BlueR startup receiver gap. Indications use the
fallback `StartNotify` path and are labelled `gatt_value_update`. GAP Device Name
advertises notification support on this firmware but has no usable notification
source; it is read instead. Failed discovery/read/subscription operations remain
in the event log. Optional `--poll-seconds N` repeats readable values; it is off
by default because this bike streams its workout data.

BlueZ property snapshots are not raw radio advertisements. For packet-level
controller evidence run alongside the CLI, before connecting:

```sh
sudo btmon -i hci1 -w /private/path/linux.btsnoop
```

`--init-command UUID=HEX` (repeatable) replaces automatic startup for controlled
protocol experiments and repeats on reconnect. Only use known commands: some
Sunny commands reset counters or change device operation. Original writes and
results are logged. No app/APK/decompiled source is redistributed by this project.

## Background service and deployment

The Debian package installs a system service, disabled by default. Configure
`/etc/sunnyfit-ble-cli/config.toml` and the root-private environment file
`/etc/default/sunnyfit-ble-cli`, then explicitly enable it:

```sh
sudo systemctl enable --now sunnyfit-ble-cli
```

See [service and storage](SERVICE.md) for multiple sinks, permissions, database
schema and logs. The legacy user service remains available for per-user capture;
use only one instance per bike.

New in 0.3: [workout views and training](WORKOUT.md), [heart rate and profiles](DEVICES.md),
and [FIT/TCX/CSV and Strava](EXPORT.md). Run `--help` for every config/ENV override.


## References

- [Debian firmware-realtek](https://packages.debian.org/trixie/firmware-realtek)
- [Official SunnyFit Android listing](https://play.google.com/store/apps/details?id=com.amoyyg.sunny)
- [BlueR 0.17.4](https://docs.rs/bluer/0.17.4/bluer/)
- [Bluetooth CSC specification](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/CSCS_v1.0/out/en/index-en.html)
- [Bluetooth Fitness Machine Service](https://www.bluetooth.com/specifications/specs/fitness-machine-service-1-0/)
- [Redis Pub/Sub semantics](https://redis.io/docs/latest/develop/pubsub/)
