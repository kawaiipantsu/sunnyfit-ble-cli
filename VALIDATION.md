# Validation record

Checked 2026-10-17 on Debian 13, x86_64. The code is version 0.4.1.
Private addresses, identities, credentials, raw captures and APK files are not
included in Git or the Debian package.

## Version 0.4.1 TUI checks

- Rustfmt, Clippy with warnings denied, 33 Rust tests and all three integration
  suites pass. Speed-history tests cover time-weighted averages, demo exclusion,
  and reading the database's precomputed lifetime speed statistics.
- All nine views render at the supported sizes. An actual 64×24 PTY check confirms
  mph, rps, miles and the window/ride/all-time statistics remain visible.
- README images were refreshed from the live demo TUI. The outline and min/max
  guides use lower-line glyphs consistently; secondary units fit in the cards.
- Lifetime SQL reads run in a separate task and feed the existing service stream.
  Attached viewers continue to need no database access. Demos never load real
  history. Missing historical fields remain unknown rather than invented.

## Version 0.4 checks

- Rustfmt, Clippy with warnings denied, and 32 Rust tests pass. New tests cover
  live socket permissions/ownership, local workout baselines and HR freshness,
  automatic five-minute trip boundaries, pause/disconnect handling, UTC calendar
  boundaries, materialized averages/totals, repeated-write idempotence and explicit
  statistics reset while retaining legacy sessions/devices.
- All three Python suites pass. The new real-process stream suite exercises
  automatic/required/direct source selection, two concurrent PTY viewers,
  independent pause and history, no duplicate database writes, raw-event replay,
  collector restart/reattachment and standalone operation. Fixtures use isolated
  sockets/configuration and synthetic telemetry without touching physical BLE.
- The installed TUI attached to the physical collector, displayed SERVICE STREAM
  and received five fresh, ready metric snapshots during a six-second test.
  Closing it left the collector PID unchanged and created no duplicate sink.
  A separate service-group account could read the socket; an unrelated local
  account was denied. This confirms live sharing, not a new speed calibration.
- The requested raw-event statistics reset was performed on MariaDB 10.11.
  Session/device records were retained. Schema 2 has no raw payload column in
  statistics. A synthetic run wrote to actual MariaDB, remote Redis and SQLite
  concurrently with zero pending records, retries or drops. Database totals
  matched the run's captured distance and active time, with one started/completed
  trip and NULL heart rate. MySQL-backed CSV, TCX and FIT exports succeeded.
- The Debian system unit creates its runtime socket directory. Installation
  preserves private configuration; this host's explicitly enabled service
  continues using MySQL and Redis. New installations remain disabled by default.
- Database exports now use minute aggregates; local recording export remains at
  one-second resolution. Calendar rollups use UTC. Automatic idle boundaries are
  fixture-tested; no physical heart-rate sensor was available.

## Version 0.3 checks

- Rustfmt and Clippy with warnings denied pass. The 24 Rust tests cover the
  original Sunny/FTMS/CSC parsing and freshness behavior, HRS flags/contact/RR
  parsing, independent sensor reconnect state, custom profile bounds, interval
  transitions, ghost interpolation, progress merging/idempotence, demo isolation,
  SQLite upserts and NULLs, export omission of unavailable fields, redacted HTTP
  errors and rendering all nine TUI views at 64×24 and 124×36.
- Both Python integration suites passed against the debug executable and the
  installed `/usr/bin/sunnyfit-ble-cli`. They exercise default config permissions,
  secret redaction, stdout modes, record/replay, SIGTERM, actual PTY keyboard
  navigation, clean JSON stdout, isolated Redis recovery, SQLite service mode,
  all three table names, database recovery, exports and a custom food catalogue.
- Actual MySQL/MariaDB and remote Redis were tested concurrently with SQLite.
  All four Redis channels received events. Database queues drained without
  drops/retries. MySQL readback and database-sourced CSV/TCX/FIT exports passed.
  Synthetic validation sessions are explicitly marked `demo` in the database.
- Garmin's official Python FIT SDK independently checked the generated FIT file's
  CRC/integrity and decoded its indoor cycling session and records. Absent heart
  rate and GPS remained absent. Exported TCX passed Garmin's official v2 XSD.
- The Debian package contains the system unit, config/environment conffiles,
  logrotate policy, profiles, fixtures, interval plan and food catalogue.
  The environment conffile is mode 0600. Installation was observed leaving the
  service disabled. It was subsequently explicitly enabled on this host at the
  owner's request, using its dedicated account and MySQL plus Redis sinks.
- `systemd-analyze verify` accepted the new unit. The installed service connected
  to the physical bike and delivered observed movement to both destinations:
  Redis showed 3.47–21.09 km/h and 13–79 rpm during one 20-second observation;
  MySQL subsequently held 222 m and 44.7 active seconds, with 72 saved metric
  snapshots at readback. A concurrent health check showed 140 writes/publishes
  per destination and zero pending records, queue drops or delivery failures.
  These values describe different observation windows, not a display calibration.
- README screenshots were captured from the actual demo TUI through a PTY.
  No personal bike identifiers or configuration are present in the images.

Rust 1.94.1 built this revision; the declared minimum 1.90 was not separately
validated. Existing physical protocol evidence below was gathered for 0.2 and
is retained alongside the new service check.

## Physical bike and adapter

The kernel already had the Realtek USB driver. Installing `firmware-realtek` and
rebinding the adapter resolved the missing `rtl8761bu_fw.bin` and made hci1 usable.

The SF-B023005DGY was discovered and its five GATT services inventoried. Device
Information reported firmware F1.0, hardware XT-2366 and software V4.0.1_251210.
All three Sunny response/status/motion channels delivered AcquireNotify data.
The advertised GAP Device Name notification source is unusable on this firmware;
it is read instead and the subscription limitation is explicitly recorded.

The bike rejected native Bluetooth bonding with `AuthenticationFailed`.
Unbonded GATT connections worked; the `connect` command verifies that connection
and persists the selected adapter/device. It does not claim a Bluetooth bond.

Static inspection of SunnyFit Android 2.19.0 established the proprietary startup,
framing and units. Live bike replies confirmed permission, metadata query and
start acknowledgements. The current startup uses A3 01 to request preservation
of device counters. An earlier reference-client experiment used A3 00 and cleared
session counters; this was disclosed to the user and is documented in
[PROTOCOL.md](docs/PROTOCOL.md). The odometer persisted across that experiment.

A movement capture produced 21.89–22.69 km/h and 82–85 rpm, then zero speed and
cadence after stopping. Its device counters reached 213 m, 44 s, 4.1 kcal and an
18,267 m odometer. An earlier user observation was approximately 22 km/h.
Exact simultaneous display comparisons for all fields remain unperformed.
Replaying the longer hardware capture produced 266 decoded frames without a
decoder error.

The final lifecycle test passed:

1. Connect, run all three acknowledged startup steps and receive motion telemetry.
2. Force a Linux disconnect; automatically reconnect, repeat startup and receive
   telemetry in a second connection epoch.
3. Run `connect` against an existing connection and persist selection.
4. Cleanly stop the recorder, forget the BlueZ device/cache, and connect again.

That recording contained 36 notifications, six startup acknowledgements across
two epochs, zero acquisition/decoding errors and no incomplete frames. It is
correctly marked **partial** because the deliberate disconnect created one gap.
Telemetry resumed about ten seconds after the forced disconnect in this run.

A simultaneous HCI trace contained 86 ATT notification payloads. All 86 matched
payload occurrences in the overlapping Rust recordings (50 from the earlier
profile capture and 36 from the final lifecycle run). This is a payload/count
cross-check over those windows, not proof of complete startup capture, radio
reception or identical ordering for every acquisition path.

## Remaining limits

- Long unattended sleep/wake operation, deliberate bike resets, display MODE and
  resistance changes, other firmware/models, and prolonged interference have not
  been exercised. Reconnect and stale-data handling are implemented, but those
  extended hardware scenarios still need observation.
- Power, electronic resistance and battery were not supplied by this bike's
  observed telemetry. Heart rate was zero/unavailable. Unknown fields are retained
  without invented interpretations; calories are device estimates.
- No physical heart-rate sensor was available: Polar, Garmin, Magene and iGPSport
  compatibility is based on their Bluetooth HRS support and synthetic protocol
  fixtures. Vendor-specific features and actual pairing/reconnect behavior still
  need hardware tests. Live Strava OAuth/upload was not exercised; request failure
  handling and export generation were tested.
- File storage, stdout, databases and Redis have bounded queues and explicit failure/loss
  accounting. Redis Pub/Sub has no retention guarantee. Crash/power-loss durability,
  disk exhaustion and saturation at extreme notification rates were not stress-tested.
- The configured APT API and token were verified with `GET <API URL>/suites`
  (HTTP 200), including the configured suite. A root-path `token_scope_required`
  response and separate documentation access failures do not mean the API is
  unavailable. The server upload/stage/publish schema was subsequently located in its local
  canonical documentation; the current client helper implements readiness checks
  and optional signing only. No APT publication of version 0.3 was performed during this validation. The signing helper follows the accessible signing
  OpenAPI; its remote execution remains untested.
- At the time of the hardware validation, Git was local. GitHub publication was
  subsequently requested by the owner. Commits use the existing global identity;
  package metadata uses a generic maintainer and contains no private credentials.
