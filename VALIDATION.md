# Validation record

Checked 2026-10-17 on Debian 13, x86_64. The code is version 0.2.0.
Private addresses, identities, credentials, raw captures and APK files are not
included in Git or the Debian package.

## Build and automated checks

- Rust 1.94.1 built the debug and optimized release executables using Cargo.lock.
  Rust 1.90 is the declared minimum; this revision was not built with that compiler.
- `make check`: rustfmt, Clippy with warnings denied, 12 Rust tests and the Python
  integration suite. The tests exercise actual executable processes.
- Protocol tests cover every fragment boundary of a captured Sunny motion frame,
  checksum/TLV corruption, partial-frame expiry, unknown tags, concatenated frames,
  the keep-counter startup flag, standard FTMS fields and truncation, CSC rollover,
  counter resets, silence, disconnects and pause boundaries.
- Integration covers first-run config creation and mode 0600, argument validation,
  secret redaction, all four verbose/debug-stdout combinations, deterministic
  record/replay decoding, SIGTERM cleanup, and the workout TUI in a pseudo-terminal
  with pause, marker and quit input. Alternate-screen restoration and clean JSON
  stdout are checked.
- An isolated localhost Redis instance verifies all four configurable channels,
  recovery when Redis is unavailable at startup, and offline replay with a
  one-record publishing queue. This does not exercise the user's production Redis
  or a downstream WebSocket consumer.
- `make deb` builds an amd64 package and SHA-256 checksum. Runtime dependencies are
  derived from the actual ELF. A staged extracted release executable passed a
  simulated capture smoke test. The package was installed locally, and the full
  Python integration suite also passed against `/usr/bin/sunnyfit-ble-cli`,
  including Redis replay with a one-record queue. The installed `doctor` command
  confirmed firmware presence and both powered adapters. The optional user service
  is not enabled by the package.

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
- File storage, stdout and Redis have bounded queues and explicit failure/loss
  accounting. Redis Pub/Sub has no retention guarantee. Crash/power-loss durability,
  disk exhaustion and saturation at extreme notification rates were not stress-tested.
- The configured APT API and token were verified with `GET <API URL>/suites`
  (HTTP 200), including the configured suite. A root-path `token_scope_required`
  response and separate documentation access failures do not mean the API is
  unavailable. Upload/stage/publish still await their request schema. No package
  was signed or published. The signing helper follows the accessible signing
  OpenAPI; its remote execution remains untested.
- Git remains local. Commits use the existing global identity; public package
  metadata uses a generic maintainer and contains no private identity or credentials.
