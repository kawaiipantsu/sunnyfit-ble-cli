# Statistics ready for websites

Schema 2 keeps the requested three tables. `workout-statistics` stores calculated
results, **not an event log**. A website can select a ready-made lifetime total,
a row for each day, a minute chart, or one trip's summary directly. The same
schema works in MySQL/MariaDB and SQLite.

## Choose one scope

Each combination of scope, bike, demo flag and time bucket/session has one row.
Current rows are updated about once per second, rather than appended for every
Bluetooth event. Calendar buckets are UTC, weeks start on Monday, and months
follow calendar boundaries. `bucket_start_ms` and `bucket_end_ms` are Unix
milliseconds; the latter is exclusive for calendar buckets. Filter `demo = 0`
for real riding. Never sum different scopes together: they are different views
of the **same** activity.

| `scope` | Ready-made result |
| --- | --- |
| `minute`, `hour` | Time-series chart buckets per bike |
| `day`, `week`, `month` | Period totals and averages per bike |
| `lifetime` | All accumulated data since statistics initialization, per bike |
| `session` | One collector run's totals (`session_id` identifies the run) |
| `trip` | One automatic workout/trip's totals (`session_id` identifies the trip) |
| `session-minute`, `trip-minute` | That session/trip's minute chart, also used for database export |

`statistic_id` is the internal upsert key. `device_id`, `session_id`, `scope`,
`demo`, and bucket times support indexed queries. `updated_ms` records the last
update, including delayed trip completion. A calendar row's start is its bucket
boundary; a session/trip summary's start/end correspond to that activity.

## Columns for dashboard cards and charts

| Columns | Meaning |
| --- | --- |
| `distance_m`, `calories_kcal` | Accumulated distance and energy **within this scope/bucket**, not a raw device counter |
| `cumulative_distance_m`, `cumulative_active_s`, `cumulative_calories_kcal` | Running session/trip totals at that chart bucket, ready for cumulative lines; NULL on global calendar rows |
| `active_s` | Observed pedaling time; excludes pauses, stale data and disconnects |
| `elapsed_s` | Collector elapsed time in calendar/session rows; trip duration in trip rows |
| `trip_elapsed_s` | Trip duration, including short internal pauses but excluding the final idle timeout |
| `speed_average_kmh`, `speed_min_kmh`, `speed_max_kmh` | Speed statistics during measured active time |
| `cadence_average_rpm`, `cadence_min_rpm`, `cadence_max_rpm` | Cadence statistics during measured active time |
| `heart_rate_average_bpm`, `heart_rate_min_bpm`, `heart_rate_max_bpm` | Heart-rate statistics when available during active time |
| `power_average_w`, `power_min_w`, `power_max_w` | Power statistics when available during active time |
| `sessions_started`, `trips_started`, `trips_completed` | Already accumulated counts; a trip counts as complete once its idle timeout or a clean shutdown closes it |
| `samples` | Number of periodic metric observations represented |

Missing distance, calories, power or heart rate stays SQL NULL until measured
or explicitly estimated; it does not become a fictitious zero. Known zero
movement remains zero. The `*_integral` and `*_observed_s` columns are internal
sums/weights used to maintain averages without reading historical rows. Published
`*_average_*` fields are already divided and ready to display. Min/max values
cover observations that contributed active measurement time, not hours of idle
zero values. Calories remain estimates from the device or configured MET/weight.

One-second contributions belong to the bucket containing their observation
end. A boundary-straddling second is not split. Short trip pauses are added to
trip elapsed time when movement resumes. Trip completion counts belong to the
bucket of the last movement, so a recent bucket can update five minutes later.
An extended database outage can exceed bounded in-memory queues; consult service
health logs for dropped/pending writes. Aggregate increments and collector
sequence checkpoints commit in one transaction, preventing retry double counts.

## Queries without scanning sessions

Dashboard cards for each bike:

```sql
SELECT device_id, distance_m / 1000.0 AS distance_km,
       active_s / 60.0 AS active_minutes, calories_kcal,
       trips_completed, speed_average_kmh, speed_max_kmh,
       cadence_average_rpm, heart_rate_average_bpm
FROM `workout-statistics`
WHERE scope = 'lifetime' AND demo = 0;
```

Daily charts (bind the desired date range as Unix milliseconds):

```sql
SELECT bucket_start_ms, distance_m, active_s, calories_kcal,
       trips_started, trips_completed, speed_average_kmh, speed_max_kmh
FROM `workout-statistics`
WHERE scope = 'day' AND demo = 0 AND device_id = ?
  AND bucket_start_ms >= ? AND bucket_start_ms < ?
ORDER BY bucket_start_ms;
```

Live minute chart with an average line and min/max band:

```sql
SELECT bucket_start_ms, speed_average_kmh, speed_min_kmh, speed_max_kmh,
       cadence_average_rpm, heart_rate_average_bpm, distance_m, active_s
FROM `workout-statistics`
WHERE scope = 'minute' AND demo = 0 AND device_id = ?
  AND bucket_start_ms >= ?
ORDER BY bucket_start_ms;
```

Latest completed workouts and their precomputed summary:

```sql
SELECT s.session_id, s.started_ms, s.ended_ms, s.elapsed_s, s.active_s,
       s.distance_m, s.calories_kcal, a.speed_average_kmh,
       a.speed_max_kmh, a.cadence_average_rpm, a.heart_rate_average_bpm
FROM `workout-sessions` AS s
LEFT JOIN `workout-statistics` AS a
  ON a.session_id = s.session_id AND a.scope = 'trip'
WHERE s.session_kind = 'trip' AND s.status = 'finished' AND s.demo = 0
ORDER BY s.started_ms DESC
LIMIT 20;
```

One workout's chart uses `scope = 'trip-minute' AND session_id = ?`.
Session/trip `distance_m` values are interval distances. For a cumulative chart,
select `cumulative_distance_m` directly; `cumulative_active_s` and
`cumulative_calories_kcal` provide the corresponding running totals.

## Automatic trips and retained sessions

Pedaling starts a trip. Five minutes without observed movement closes it at the
last movement timestamp, excluding the trailing wait. Short pauses remain in
the same trip. Configure `--trip-idle-seconds 300` or
`SUNNY_TRIP_IDLE_SECONDS=300`; `0` disables automatic trips. Restarting a
collector starts a new collector session and new trip detection. Clean shutdown
finishes an open trip with `collector_stopped`; a crash can leave `recording`
rows, distinguishable by stale update times.

`workout-sessions.session_kind` distinguishes `collector` from `trip`;
`parent_session_id` links a trip to its collector. Do not sum both kinds together.
The service remains the sole database writer when a TUI attaches to it. TUI
workouts have independent local history and do not double-count these aggregates.

On upgrade from schema 1, explicitly reset the old statistics table as described
in [the service guide](SERVICE.md#three-table-database-layout). Session/device
records are retained. Their old totals are not silently folded into new totals;
the statistics begin at reset. Raw BLE captures remain available through local
recording and Redis. Database FIT/TCX/CSV export uses minute aggregates; use a
local recording when one-second export detail is required.
