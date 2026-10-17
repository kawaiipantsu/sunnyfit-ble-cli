# Connector service and storage

Version 0.3 installs `sunnyfit-ble-cli.service` as a **system service**, disabled by
default. Installation creates a dedicated service account; it does not start
Bluetooth acquisition. The optional user unit from earlier versions is retained.
The service owns the Bluetooth connection. From version 0.4, `--workout`
automatically attaches to its local live stream while collection continues.
Without a service, the same command connects to Bluetooth independently.

```sh
sudoedit /etc/sunnyfit-ble-cli/config.toml
sudoedit /etc/default/sunnyfit-ble-cli
sudo systemctl enable --now sunnyfit-ble-cli
sudo systemctl status sunnyfit-ble-cli
sudo tail -f /var/log/sunnyfit-ble-cli.log
# Open a workout while the service keeps collecting:
sunnyfit-ble-cli --workout
```

Set `device` and `adapter` using discovery results. A minimal environment file:

```ini
SUNNY_DEVICE=AA:BB:CC:DD:EE:FF
SUNNY_ADAPTER=hci1
SUNNY_SINKS=mysql,redis
SUNNY_MYSQL_URL=mysql://USER:URL_ENCODED_PASSWORD@DB_HOST:3306/DATABASE
SUNNY_REDIS_URL=redis://REDIS_HOST:6379/0
# Optional:
# SUNNY_SINKS=mysql,redis,sqlite
# SUNNY_SQLITE_PATH=/var/lib/sunnyfit-ble-cli/workouts.sqlite3
# SUNNY_HEART_RATE_DEVICE=AA:BB:CC:DD:EE:00
```

Keep `/etc/default/sunnyfit-ble-cli` root-owned, mode **0600**. systemd reads it
before dropping privileges; the service account does not need to read that file.
URL-encode reserved characters in credentials. Do not put credentials into Git,
issue reports, shell command arguments or screenshots. Restart the service after
changing configuration. The package defaults to SQLite until configured.

`--systemd` disables raw/debug stdout and removes the `local` sink. It requires at
least one configured external sink and rejects `--workout`. It writes lifecycle
and delivery counters to `/var/log/sunnyfit-ble-cli.log`, with packaged daily
rotation, and creates its default configuration under the service home. It does
not create `past-workouts`, achievements, games or desktop notifications.
The service's writable state is confined to `/var/lib/sunnyfit-ble-cli`, its
log file and `/run/sunnyfit-ble-cli` for the live socket. Keep custom SQLite paths inside that directory, or explicitly adjust
`ReadWritePaths` in a systemd override.

CLI, environment and TOML use the same settings: **CLI > ENV > config > defaults**.
For example `--sinks mysql,redis,sqlite` overrides `SUNNY_SINKS`, which overrides
`sinks = ["sqlite"]`. Use `--help` for every flag and its environment name.
Redis channel names remain independently configurable. Regular CLI capture
uses `local,redis` by default; Redis is inactive without a URL. Use `--sinks` to
select exactly the required destinations. The application also supports quiet
foreground operation with external sinks without `--systemd`.

## Sharing the service with a workout

The collector publishes a read-only Unix socket at
`/run/sunnyfit-ble-cli/stream.sock`. No Redis connection or database credentials
are required by a viewer. The system unit creates a runtime directory (0750);
the socket is owned by the service account/group and has mode 0660. For a regular
login account, grant access once and then log out and back in:

```sh
sudo usermod -aG sunnyfit-ble-cli "$(id -un)"
sunnyfit-ble-cli --workout
```

Root already has access. The TUI displays **SERVICE STREAM**, starts its own
workout timer and distance baseline, and saves local history when the `local`
sink is selected (the interactive default). MySQL/Redis/SQLite writes stay with
the collector, even if the viewer's configuration also selects those sinks.
Pausing or quitting a viewer does not pause acquisition or other viewers.
Bluetooth, protocol and heart-rate sensor selection belong to the service;
training, goals, routes and display settings belong to each viewer. Service
calorie/distance estimates are inherited when available; user MET estimates can
fill missing calories locally.

| Setting | Behavior |
| --- | --- |
| `--workout-source auto` / `SUNNY_WORKOUT_SOURCE=auto` | Default: attach when available, otherwise connect directly to Bluetooth |
| `--workout-source service` | Require the local stream; fail clearly if it is unavailable |
| `--workout-source direct` | Use Bluetooth; reject a known service connection to the same bike |
| `--stream-socket PATH` / `SUNNY_STREAM_SOCKET` | Override the socket path on both collector and viewer |

Permission errors, an incompatible stream or a running service with a missing
socket produce an actionable error instead of a competing BLE connection.
If selecting a different bike while the service is active, use `direct` with
that bike's address. Stop the service before taking its bike over directly.
An attached viewer waits and reattaches after service restarts; it never takes
over Bluetooth during a stream outage. Counter baselines reset across gaps, so
unobserved riding is excluded from the local workout. Slow subscribers have
bounded queues and may reconnect with a recorded coverage gap. Up to 32 viewers
are accepted. Local capture includes service snapshots and original events
observed after attachment; earlier raw acquisition remains in service storage.
`--debug-stdout -v` includes these records, and `replay` understands the nested
original events.

An older service executable must be restarted after upgrading before a viewer
can attach. Custom sockets for manually launched collectors should live in a
private or deliberately shared directory. The legacy user unit performs direct
capture and does not expose this system-mode stream.

## Three-table database layout

MySQL/MariaDB and SQLite use the same **schema version 2**. The configured
database must already exist. The connector needs CREATE, INDEX, ALTER, SELECT,
INSERT and UPDATE privileges; an explicit statistics reset also needs DROP.

| Table | Contents |
| --- | --- |
| `workout-devices` | Device role/profile, last-seen time and metadata |
| `workout-sessions` | Collector runs and automatic trips, with start/end/status, elapsed/active time, distance/calories and parent collector ID |
| `workout-statistics` | Materialized minute/hour/day/week/month/lifetime totals, session/trip totals and their minute chart buckets; averages, minima/maxima, counts and nullable measurements |

`workout-statistics` contains **aggregates**, not raw BLE events. Read a daily or
lifetime row directly for a dashboard; no scan of all sessions or JSON decoding
is needed. Averages are weighted by active seconds with a known measurement.
The service refreshes current buckets about once per second. Calendar buckets
use UTC and each device/demo flag has separate totals. SQL queries and all
column meanings are in the [statistics guide](STATISTICS.md).

Automatic trips start on observed pedaling and finish after five minutes idle.
Their recorded end is the last movement, excluding the five-minute wait.
`--trip-idle-seconds` / `SUNNY_TRIP_IDLE_SECONDS` changes the threshold; zero
disables trip detection. Short pauses stay in the same trip. Normal collector
shutdown finishes an open trip with reason `collector_stopped`. A crash can
leave a recording session/trip open; a new collector starts new trip detection.
The service itself remains one collector session until restarted. Attached TUI
workouts retain their own local history without duplicating database totals.

Version 0.3's raw-event statistics table needs an **explicit reset** before using
schema 2; it is never silently deleted. Stop the collector, then run the reset
with the selected database's private configuration:

```sh
sunnyfit-ble-cli --config /path/to/private-config.toml reset-statistics \
  --database mysql --confirm-reset
```

This deletes only `workout-statistics`. Existing session/device rows are retained
and session columns are added as needed. Aggregates start from subsequent
acquisition, without reconstructing or double-counting old sessions. Restart the
updated service afterward. Do not reset statistics while a collector is writing.
Raw data is still available through Redis channels or local capture/replay.
Database-based exports use minute aggregates; local recording exports retain
one-second measurements. Historical sessions whose statistics were reset no
longer have a database timeline to export.

Each destination has an independent bounded queue. Database writes use
transactions with a per-collector sequence checkpoint; reconnect retries cannot
increment aggregate totals twice. Outages do not block BLE reception. Queue overflow, retries and
pending records appear in service health logs and final summaries. Queues are in
memory, so extended outages, termination deadlines or power loss can lose pending
events. Redis Pub/Sub also requires subscribers to be connected; a successful
publish does not mean a consumer stored the message. Add SQLite alongside remote
sinks when you need a local archive. The default Redis build supports plain Redis,
not `rediss://`; use a trusted network or a transport tunnel.

```sh
sunnyfit-ble-cli sessions --database sqlite
sunnyfit-ble-cli --sqlite-path /path/to/workouts.sqlite3 sessions
# Supply SUNNY_MYSQL_URL privately for these commands:
sunnyfit-ble-cli sessions --database mysql
sunnyfit-ble-cli export --database mysql --session UUID --format fit --output ride.fit
```

Service-owned SQLite is private. Query/export it as the service user or root.
Package removal stops the service. Data and the service account are preserved,
even on purge. Package upgrades do not automatically restart acquisition; restart
at a convenient point to use the updated executable.
