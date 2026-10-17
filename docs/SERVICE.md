# Connector service and storage

Version 0.3 installs `sunnyfit-ble-cli.service` as a **system service**, disabled by
default. Installation creates a dedicated service account; it does not start
Bluetooth acquisition. The optional user unit from earlier versions is retained.
Use one instance per bike. Stop the system service before starting a direct
interactive workout against the same bike.

```sh
sudoedit /etc/sunnyfit-ble-cli/config.toml
sudoedit /etc/default/sunnyfit-ble-cli
sudo systemctl enable --now sunnyfit-ble-cli
sudo systemctl status sunnyfit-ble-cli
sudo tail -f /var/log/sunnyfit-ble-cli.log
# To switch to an interactive workout:
sudo systemctl stop sunnyfit-ble-cli
sunnyfit-ble-cli --workout
sudo systemctl start sunnyfit-ble-cli
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
The service's writable state is confined to `/var/lib/sunnyfit-ble-cli` and its
log file. Keep custom SQLite paths inside that directory, or explicitly adjust
`ReadWritePaths` in a systemd override.

CLI, environment and TOML use the same settings: **CLI > ENV > config > defaults**.
For example `--sinks mysql,redis,sqlite` overrides `SUNNY_SINKS`, which overrides
`sinks = ["sqlite"]`. Use `--help` for every flag and its environment name.
Redis channel names remain independently configurable. Regular CLI capture
uses `local,redis` by default; Redis is inactive without a URL. Use `--sinks` to
select exactly the required destinations. The application also supports quiet
foreground operation with external sinks without `--systemd`.

## Three-table database layout

MySQL/MariaDB and SQLite use the same schema and SI/unit-bearing column names.
The configured database must already exist; the application creates its tables
and needs CREATE, SELECT, INSERT and UPDATE privileges. Schema version 1 is
recorded in the device and session rows.

| Table | Contents |
| --- | --- |
| `workout-devices` | Stable role/address key, bike or heart-rate role, profile, last-seen timestamp and device metadata JSON |
| `workout-sessions` | One connector/workout run per UUID; start/end timestamps, mode, demo flag, status, elapsed/active time, distance, calories and latest workout JSON |
| `workout-statistics` | Every event keyed by session UUID and sequence, wall-clock/elapsed timestamps, event type, device role, nullable metrics and complete event JSON |

Names contain hyphens: quote them with backticks in SQL. On `metrics` rows,
distance and calories are session totals; on `decoded` rows they may be raw
cumulative device counters. Use `event_type='metrics'` for charts. The complete
JSON retains provenance, unknown tags, RR intervals and raw bytes. Missing power
or heart rate is SQL NULL. A continuously running service is one session until
it restarts; it does not infer separate workouts from pauses at the desk.
An unclean shutdown can leave a session marked `recording`, with no end timestamp.

Each destination has an independent bounded queue. Database writes use
transactions and idempotent `(session_id, sequence)` keys; reconnect retries cannot
duplicate a row. Outages do not block BLE reception. Queue overflow, retries and
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
