# Validation record

Checked October 16, 2026.

## Confirmed target and continuation

The user confirmed SF-B023005DGY and requested collection of all data the bike
exposes. `CODEX_HANDOFF.md` defines that goal and the remaining implementation
and hardware work. The handoff update changes documentation only; the Rust
source and dependency lockfile retain the previously validated baseline.

## Completed

- Built the final source successfully on Linux x86_64 using Rust 1.90.0 with
  `cargo build --offline --features vendored-dbus`. The vendored D-Bus C library
  and all Rust dependencies compiled. Cargo.lock contains the resolved versions.
- Formatted the source using Rust 1.90.0's rustfmt.
- Ran the compiled binary's top-level, scan, inspect, and capture help commands;
  all returned exit code 0.
- Verified the compiled CLI rejects an invalid Bluetooth address, zero scan
  duration, zero capture duration, an out-of-range characteristic selector, and
  an ambiguous selector. All returned the expected argument-error exit code 2.
- Verified that capture help does not expose the value of SUNNY_REDIS_URL when
  the environment contains a credential-bearing synthetic URL.
- Reviewed the BLE API usage, notification setup and instance identification,
  interrupted-connection cleanup, raw-data preservation, Redis backpressure,
  publication failure accounting, and timestamp semantics against the source
  and current library documentation.

These are build, CLI smoke checks, and source review. They are not hardware
integration tests or tests of a decoded SunnyFit protocol. There is no packet
decoder in this version and no claim of measured compatibility with a bike.

## Hardware and integration checks still required

1. Discover the intended bike on the user's Linux Bluetooth adapter.
2. Record the confirmed SF-B023005DGY's observed identity, service UUIDs,
   characteristic properties, and firmware information if available.
3. Establish whether pairing is needed.
4. Confirm that selected subscriptions deliver raw bytes and identify any
   required application initialization exchange.
5. Compare the Rust capture with an HCI trace for packet completeness and
   ordering during subscription startup.
6. Verify Pub/Sub delivery against the user's Redis server and subscriber.
7. Validate future decoded metrics against controlled pedal sessions and
   display readings, including stop, resume, reset, and reconnect behavior.

## Deliberate scope limits

- No automatic BLE or Redis reconnect.
- No proprietary SunnyFit commands, packet decoder, or standard FTMS/CSC
  decoder is implemented yet.
- Notification events are BlueZ value updates; timestamps and sequence are
  host ingestion metadata. See README.md for the startup-capture limitation.
- Local stdout capture depends on its consumer keeping up and does not fsync
  each line. Redis Pub/Sub does not retain history.
- Redis TLS features, Redis Streams, a latest-metric state key, systemd
  deployment, a web backend, and a browser frontend are outside this starter.
