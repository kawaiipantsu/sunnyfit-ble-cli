# Codex handoff: collect the Sunny SF-B023005DGY's Bluetooth data

## 1. User request and confirmed context

Continue this project on the user's Linux host, where the physical bike and a
Bluetooth adapter will be available. Build and validate a small Rust CLI that
connects to the bike, collects every observable Bluetooth data source exposed
by it, preserves the original data, and makes that data available to the user's
own applications.

The user confirmed the model on October 16, 2026:

**Sunny Health & Fitness Smart Under Desk Mini Exercise Bike, SF-B023005DGY.**

Treat that model as confirmed. Its firmware, advertised name/address, GATT
database, pairing requirements, and application protocol still need to be
observed on the host. Do not ask the user to reconfirm the model merely because
older research described it as a likely match.

The user's intended architecture is:

- Linux Rust CLI for Bluetooth acquisition and decoding.
- Redis for downstream distribution, with Pub/Sub for live updates.
- A web backend that forwards updates through a WebSocket to a browser.

The immediate priority is the Rust collector and the bike's data. The user
specifically wants **all the data the bike produces**, including fields that
we do not yet understand. Do not stop after obtaining one speed or cadence
value, and do not build a dashboard as a substitute for completing acquisition.

This document is a project handoff, not evidence that hardware tests have
already passed. The earlier environment had no access to the bike. Continue
with implementation and local investigation; ask for physical actions when
needed, such as pedaling, operating the display, or making a phone capture.
Use the host's existing permission and approval mechanisms for privileged
operations, and preserve any existing repository work and instructions.

## 2. Define what “all data” means and how to report it

Collect all bike-related payloads and readable values exposed through the
observed Bluetooth interface across a documented set of operating conditions.
Preserve both bike-to-host data and the host requests necessary to interpret
that data. Keep unknown fields, unrecognized messages, repeated messages, and
original packet boundaries.

Track two separate results:

1. **Acquisition coverage:** which sources and operating states were examined,
   which data was recorded, and where acquisition was incomplete.
2. **Decoding coverage:** which fields have a validated meaning, which are only
   correlated with a metric, and which remain unknown.

A successful subscription is not proof of complete acquisition. Capturing all
updates on one characteristic is not proof that it is the only data source.
A silent characteristic remains “silent in the tested conditions,” not “unused.”

“All” cannot establish the absence of hidden firmware behavior, or recover
measurements that the bike never exposes. HCI capture records what the host's
controller reports; it is not proof that every radio transmission was received.
State this boundary in the final coverage report rather than claiming absolute
radio-level completeness.

Initial comparison targets from the manufacturer's meter documentation are
time, speed, distance, calories, and odometer. Cadence, resistance, battery
state, device status, firmware information, and additional counters are
discovery targets, not assumed capabilities. A display or app value may be
calculated from another field instead of transmitted directly. [1][2]

## 3. Starting project and completed checks

The accompanying archive extracts to `sunny-ble/` and contains:

| File | Purpose |
| --- | --- |
| `Cargo.toml` and `Cargo.lock` | Rust project and resolved dependency versions |
| `src/main.rs` | Current discovery, inspection, capture, and Redis implementation |
| `README.md` | Existing commands, setup guide, and research notes |
| `VALIDATION.md` | Completed checks and hardware validation still required |
| `CODEX_HANDOFF.md` | This continuation brief |
| `LICENSE` | Project license |

Read these files before changing the implementation. If only this Markdown
file is available, locate the starter archive or ask the user to supply it;
do not assume the project's code is embedded in this document.

The starter uses Rust edition 2021, supports Rust 1.90 or newer, and pins
`bluer` to 0.17.4 with `bluetoothd` and `serde`. It also uses Tokio, Clap,
Futures, Serde JSON, Redis, Anyhow, and UUID. The optional `vendored-dbus`
feature builds the D-Bus client library from source. Keep this working baseline
unless an observed need justifies changing dependencies. [5]

Checks already performed on the source provided:

- Successful Linux x86_64 build with Rust 1.90.0 and `vendored-dbus`.
- Rustfmt formatting and lockfile consistency checks.
- Successful help output for the top level and all three commands.
- Rejection of invalid Bluetooth addresses, zero durations, out-of-range
  selectors, and malformed selectors.
- Help output does not disclose a credential-bearing Redis environment value.
- Source review of connection cleanup, per-instance identification, partial
  subscription handling, timing semantics, and Redis failure accounting.

No bike connection, physical-data capture, live Redis integration, or metric
decoding has been validated. The latest package update adds this handoff and
confirms the model in documentation; it does not upgrade the collector to the
comprehensive recorder specified below.

## 4. Current CLI behavior and remaining gaps

These commands exist now:

```bash
sunny-ble [--adapter hci0] scan --seconds 20
sunny-ble inspect ADDRESS [--read] [--pair]
sunny-ble capture ADDRESS [--seconds 90] [--pair] [--select SERVICE_ID:CHAR_ID]
```

`--select` can be repeated; the IDs are decimal values from the inventory.
Global `--timeout-seconds` defaults to 20 seconds per setup/read operation.
Capture optionally uses `SUNNY_REDIS_URL` or `--redis-url`; its default channel
is `sunny:bike:raw` and `--channel` overrides it.

The following table is the implementation backlog, not a list of completed
features:

| Area | Current behavior | Work needed for the user's full goal |
| --- | --- | --- |
| Advertisements | Emits deduplicated BlueZ device-property snapshots, including manufacturer/service data | Retain repeated received reports and raw AD bytes, distinguish advertisements and scan responses where exposed, and document controller/API limits |
| Inventory | Enumerates service, characteristic, and descriptor UUIDs and local IDs | Preserve full metadata, record partial discovery failures, investigate readable descriptor values, and retain ATT handle mappings when available |
| Reads | `inspect --read` performs a separate one-time pass over readable characteristics | Record requests/results as raw events; identify values that change only when read; add measured polling only where needed |
| Subscriptions | Attempts all notify/indicate candidates; logs failures and continues if one succeeds | Store every subscription result in a structured coverage report; do not present partial subscription coverage as complete |
| Startup | Uses `bluer::Characteristic::notify()` | Investigate its first-update setup gap; establish receiver readiness before confirmed initialization writes and validate against HCI |
| Timing | Timestamps updates when the merged stream is consumed | Preserve capture/processing timestamps with honest names and HCI correlation; merged sequence is not exact wire order |
| Source attribution | Calls updates `gatt_value_update` | Distinguish explicit reads from actual notifications/indications using available evidence, especially if polling is introduced |
| Lifecycle | Unexpected disconnect ends capture | Add reconnect, fresh discovery, fresh subscriptions, verified initialization, connection epochs, and explicit coverage-gap records |
| Storage | Writes/flushed JSONL to stdout before Redis queueing | Add managed capture files, experiment markers, error/loss accounting, a durability policy, and replay |
| Redis | Bounded queue/timeouts; failures do not block Bluetooth acquisition | Add recovery/replay after recording is reliable; retain raw and decoded channels separately; Pub/Sub is not an archive |
| Protocol | No decoder or vendor initialization | Discover and validate this model's actual exchange, then implement documented decoders without discarding unknown bytes |

The existing `scan` intentionally deduplicates identical snapshots. Removing
that deduplication alone does not make BlueZ device properties equivalent to
raw advertisement packets. Use an appropriate acquisition interface or the HCI
trace to retain packet boundaries and repeated reports.

BlueR 0.17.4's `notify()` starts the notification session before registering its
value-event receiver. An immediate first update could therefore be missed; this
is an inference from source order, not an observed defect on this bike. Its
stream also reports cached-value changes after reads. Preserve an HCI baseline
and do not label every such change an unsolicited radio notification. [6]

## 5. First actions on the Linux host

### 5.1 Inspect the project and host

Establish the repository state, installed Rust version, Linux/BlueZ versions,
available adapters, adapter power, system-bus access, and existing relevant
tools. Record the actual adapter and source revision in the run manifest.

Useful initial commands:

```bash
rustc --version
cargo --version
uname -srmo
bluetoothctl --version
bluetoothctl list
bluetoothctl show
```

Install only missing prerequisites appropriate to the distribution and host
permissions. On Debian/Ubuntu the usual packages are `bluez`,
`libdbus-1-dev`, `pkg-config`, and `build-essential`. The README contains
example installation commands. A vendored build still requires a working
system D-Bus, BlueZ, Bluetooth adapter, and a C compiler.

Build and run help:

```bash
cargo build --release --locked
./target/release/sunny-ble --help
```

Alternatively:

```bash
cargo build --release --locked --features vendored-dbus
```

Use the existing project's behavior to establish a baseline before changing
protocol logic. Run it on the host initially; containerization is secondary to
confirming hardware access and data acquisition.

### 5.2 Capture before discovery

Create a new directory for each experiment. Use UTC timestamps in filenames
and records, and note any local-time display/video offset. Record the host's
actual timezone and clock status in the run manifest.

For example, choose one run directory and reuse its exact path in both terminals:

```bash
mkdir -p captures/run-001
```

Start HCI recording in terminal A before scanning or connecting:

```bash
sudo btmon -i hci0 -w captures/run-001/linux.btsnoop
```

Use the actual adapter in place of `hci0`. In terminal B, wake the bike and
disconnect the phone's SunnyFit connection, then run:

```bash
./target/release/sunny-ble --adapter hci0 scan --seconds 30 \
  > captures/run-001/scan.jsonl 2> captures/run-001/scan.log
```

Identify the bike from observed advertisements and the confirmed model. Do
not assume its address or that its name contains “Sunny.” Cached BlueZ entries
are not evidence of current radio presence.

After choosing the actual address:

```bash
sunny_bike_address='REPLACE_WITH_THE_DISCOVERED_ADDRESS'

./target/release/sunny-ble --adapter hci0 inspect "$sunny_bike_address" --read \
  > captures/run-001/gatt.jsonl 2> captures/run-001/inspect.log

./target/release/sunny-ble --adapter hci0 capture "$sunny_bike_address" --seconds 120 \
  > captures/run-001/ride.jsonl 2> captures/run-001/ride.log
```

Watch the diagnostic log for readiness before starting timed pedal stages.
Record command exit statuses and subscription failures. Stop `btmon` after the
experiment and preserve the original trace. The current commands may create
separate connections for inspection and capture; the HCI trace should cover
the entire sequence. [7]

Start without `--pair`. Add pairing if an observed security requirement calls
for it. Bluetooth bonding and a SunnyFit application handshake are separate
questions. Use interactive `bluetoothctl` pairing if a PIN/passkey is needed.

### 5.3 Produce the first concrete result

Before claiming device support, produce:

- The complete discovered GATT inventory, with explicit errors or omissions.
- A short raw capture with synchronized action/display notes.
- The HCI trace covering discovery through the end of capture.
- An initial coverage table identifying every discovered data source.
- A written answer to whether data streams without application writes.

If no values arrive, keep the evidence and investigate awake state,
subscription errors, readable/polled values, advertisement data, pairing, and
application initialization. Do not equate silence with encryption or lack of
support.

## 6. Build the complete recorder before optimizing decoded output

### 6.1 Managed recording and event schema

Add a managed recording mode/output directory while keeping the existing
commands useful. The exact new flags and module boundaries are implementation
choices; they do not exist yet.

A run should contain at least:

| Artifact | Contents |
| --- | --- |
| `manifest.json` | Model, observed identities, firmware when available, host/adapter/software versions, run scope and configuration |
| `inventory.json` or `inventory.jsonl` | Services, characteristic properties, descriptors, available handles/IDs, and every discovery/read error |
| `events.jsonl` | Original data, reads/writes, subscription outcomes, connection state, markers, and recorder errors |
| `linux.btsnoop` | HCI evidence, or a reference to the trace stored for the run |
| `observations.md` | Timed physical actions, display values/units, and manually counted revolutions |
| `coverage.md` plus structured summary | Source-by-source and scenario-by-scenario coverage with outstanding gaps |
| `decoded.jsonl` when a decoder exists | Derived view that always refers back to raw event IDs |

Do not overwrite earlier runs. Keep raw personal captures local and outside
normal source-control commits. Use small intentionally selected fixtures for
decoder tests.

Each event should include, where available:

- Schema version, acquisition `run_id`, connection epoch, and event sequence.
- Host observation time, monotonic elapsed time, and processing time if distinct.
- Direction: host-to-bike, bike-to-host, or local recorder event.
- Kind: advertisement, scan response, read request/result, write request/result,
  notification, indication, subscription outcome, connection event, marker,
  recorder failure, or an honestly labeled generic value update.
- Adapter, observed address and address type, and the logical bike identity.
- Service/characteristic UUIDs plus per-instance IDs and ATT handles when known.
- Descriptor identity for descriptor operations.
- Exact payload bytes and length; preserve framing and repetitions.
- Outcome/error details and an explicit reference to any request being answered.

Keep fields absent when the chosen interface does not expose them. Do not
invent radio timestamps, ATT handles, notification/indication distinctions, or
exact ordering from BlueZ's merged property updates.

Add timestamped experiment markers to the same timeline, so phrases such as
“pedaling started,” “MODE pressed,” and “reset” are not reconstructed from
memory. Mark the recorder's ready point explicitly.

### 6.2 Persistence and loss accounting

Make local recording independent of Redis and decoding. Use a bounded writer
handoff with an explicit policy for overload; no bounded design can guarantee
retention if its sink cannot keep up indefinitely.

Assign local event IDs before the handoff. Detect queue overflow, failed
writes, disk exhaustion, decoder errors, and incomplete shutdown. Preserve a
partial-capture status when any of these compromise recording. Define and
document a durability policy, such as periodic `sync_data` plus a final sync;
ordinary stdout flush is not a guarantee against a host crash.

Keep known local drops distinct from unknown losses during disconnection,
controller filtering, or before API delivery. A gap-free host-generated
sequence does not prove that no Bluetooth packet was lost upstream.

Handle Ctrl-C and SIGTERM, drain or account for queued records, close files,
and write a final summary. Report **complete, partial, or failed relative to
the run's declared capture plan**, with explanations. Do not label a partial
capture complete merely because some values were received.

### 6.3 Cover each Bluetooth data source

Investigate all of the following, recording explicit negative outcomes:

1. **Advertisements and scan responses.** Keep raw received reports and repeats.
   Use HCI as the initial evidence when the high-level API cannot expose them.
   Observe before connection, during connection if technically supported, after
   disconnection, and through sleep/wake. The bike may stop advertising while
   connected; document observed behavior.
2. **Readable characteristics.** Record exact request/result bytes. Repeat
   candidate dynamic values at marked experiment boundaries. Poll only sources
   shown to need polling, at a measured interval recorded in the manifest.
3. **Descriptors.** Record UUIDs and metadata and attempt appropriate reads
   where supported. Some APIs do not expose descriptor-read capability flags;
   preserve the actual denial/unsupported/timeout outcome. Do not assume every
   descriptor can be read or write descriptor values speculatively.
4. **Notifications and indications.** Account for every discovered source and
   its subscription outcome. Retain unknown messages and continue recording
   when a decoder does not recognize them.
5. **Status and metadata.** Retain feature values, device information, battery
   data, counters, status/control responses, and connection changes if present.
6. **App-triggered data.** Investigate traffic enabled by connecting SunnyFit,
   starting/pausing/finishing a workout, and periodic polling or keepalives.

Preserve initial raw updates while other subscriptions are being established.
Investigate a receiver registered before StartNotify, or `notify_io()` on
compatible notifying characteristics. `notify_io()` is not a universal drop-in
replacement: indications and availability/exclusivity constraints need their
own handling. Establish the receive and recorder paths before any verified
initialization writes, then compare the results with HCI. [6]

### 6.4 Reconnect and identity

Add bounded exponential backoff for transient disconnection and explicit
connected/disconnected/retrying events. Reidentify the intended bike when an
address changes; do not connect to a similarly named nearby device solely by
name. Record the evidence used to associate an observation with this bike.

After reconnecting, discard stale objects/IDs, rediscover services, recreate
subscriptions, and repeat only the initialization verified for this model.
Record the disconnected interval as a coverage gap. Keep acquisition-run
identity, connection epochs, and workout/counter continuity separate. A new
connection does not imply a reset of the bike's accumulated counters.

Silence can be legitimate while the bike is stopped. Represent silence/stale
data and disconnection separately; do not repeatedly emit the last value as a
fresh measurement or replace unknown data with a measured zero.

## 7. Protocol identification and existing research

Use the actual connected GATT inventory to choose a protocol. Candidate
standard UUIDs are: [3][4]

| Service | Service UUID | Measurement characteristic |
| --- | --- | --- |
| Fitness Machine Service | `0x1826` | Indoor Bike Data `0x2AD2` |
| Cycling Speed and Cadence | `0x1816` | CSC Measurement `0x2A5B` |
| Cycling Power | `0x1818` | Cycling Power Measurement `0x2A63` |

The full standard base UUID format is, for example,
`00002ad2-0000-1000-8000-00805f9b34fb` for Indoor Bike Data.

An existing Swift SunnyFit implementation defines this proprietary service:

```text
FD710001-E950-458E-8A4D-A1CBC5AA4CCE
```

It defines these characteristics using the same suffix: [8]

| Prefix | Role in the existing code |
| --- | --- |
| `FD710002` | Command |
| `FD710003` | Notification 1 |
| `FD710004` | Notification 2 |
| `FD710005` | Command 2 |
| `FD710006` | Notification 3 |

These UUIDs are evidence from other Sunny equipment, not a confirmed GATT map
for SF-B023005DGY. The user confirmed the physical model, not its protocol.

The Swift project and QZ's SunnyFit stepper code are useful references. They
disagree about some message framing and field interpretations. Do not adopt
their metric offsets or startup writes as bike defaults. Preserve individual
updates so later evidence can resolve whether they are fragments or separate
records. QZ's referenced file is explicitly a stepper implementation. [8][9]

A captured test for a different Sunny model, SF-E902 SMART-H, advertises FTMS
service data. This reinforces the need for discovery instead of assuming
“SunnyFit” means one transport profile. [10]

### If an application handshake is required

Record a successful official SunnyFit session from before connection through
workout start, pedaling, pause, finish, and disconnect. Android's developer
options can enable Bluetooth HCI snoop logging; restart Bluetooth after
enabling it and obtain the available trace through logs or a bug report.
Locations/report contents vary. AOSP documents `btsnooz.py` for embedded
traces. [11][12]

Linux `btmon` records the Linux adapter's traffic. It does not capture the
nearby phone's independent connection. If the user has no suitable phone trace,
work with the evidence available and record that limitation rather than
pretending to have observed the app exchange.

Inspect the bike-specific ATT traffic in Wireshark. Preserve:

- Subscription/descriptor configuration and any reads.
- Each write's destination, payload, and write-with/without-response type.
- Responses, ordering, timing dependencies, polls, and keepalives.
- Differences between connection, workout start, pause, finish, and reconnect.

Build a transcript and implement only the exchanges supported by this model's
evidence. Model them as documented protocol steps with expected responses and
timeouts. Keep outgoing bytes in the raw record. Matching a stepper UUID by
itself is not sufficient justification for replaying every stepper command.
Command discovery should follow observed traffic and controlled experiments,
not indiscriminate writes to every writable characteristic.

Extract bike-specific traces before sharing; full phone bug reports may contain
unrelated personal diagnostics. No cloud SunnyFit account integration is
required for the initial Linux BLE investigation.

## 8. Experiment matrix

Use clearly marked runs, keep original files, and change one variable at a
time. Record displayed units and approximate cadence independently with a
known number of full crank revolutions over a timed interval. Ask the user to
perform physical steps when the recorder is ready.

| Scenario | Procedure | Questions to resolve |
| --- | --- | --- |
| Awake and disconnected | Observe about 30 seconds of advertisements with the bike still | Identity, repetition, raw AD fields, idle data |
| Connection without vendor writes | Discover, read, subscribe, remain still 30-60 seconds | Automatic streaming, initial values, heartbeat, status |
| First movement | Pedal a small counted number of complete revolutions | Movement-triggered channels, count increments, pulses per revolution |
| Cadence stages | Three comfortable, distinct rates for 30-60 seconds each | Scale, update rate, smoothing, lag, candidate cadence/speed fields |
| Stop | Remain still at least 30 seconds while connected | Zero versus stale values, continued timer, delayed stop detection |
| Resume | Resume without resetting | Counter continuity and pause behavior |
| Resistance change | Repeat similar cadence at two recorded knob positions | Whether any transmitted field responds to resistance |
| Display MODE changes | Cycle through display modes while recording | Whether selection changes Bluetooth data or reporting |
| Session reset | Preserve current readings, then deliberately reset using the documented meter control in a separate run | Which counters reset, which persist, reset/status traffic |
| Reconnect while awake | Disconnect/reconnect without resetting | Counter continuity, identity changes, required initialization |
| Sleep/wake | Observe roughly six minutes without input, then wake the bike | Behavior beyond the manual's four-minute idle timeout, reconnection and retained counters |
| Official-app workout | Record app connection, start, pedal, pause, finish, disconnect separately | Additional sources, initialization, polling, app-dependent modes |
| CLI reproduction | Repeat the required observed exchange and key physical tests | Whether the independent collector receives equivalent device data |

The four-minute sleep behavior and the documented display/reset behavior are
from the manufacturer's manual. Preserve existing counter readings before any
reset experiment. Battery removal is not required for the initial milestone;
the manual says it resets the odometer, so treat it as a separate optional
experiment rather than a routine reconnection method. [2]

Record “no change observed under this experiment” precisely. For example,
unchanged bytes during one resistance test do not prove the absence of a
resistance field under every possible state.

## 9. Decoding, replay, and metric provenance

Build the decoder independently of Bluetooth I/O. Saved raw files must be
replayable without the bike or Redis. Retain an event reference on each decoded
result so every interpretation can be traced to the original bytes.

For standard services, follow the published format and feature/presence flags.
Do not parse FTMS using unconditional offsets. Where counters and event times
are used, handle wraparound, repeated timestamps, stop detection, reset, and
reconnect continuity. [4]

For proprietary records, establish message type, framing, lengths, endianness,
signedness, scale, units, optional fields, and counter scope from multiple
observations. Do not infer a checksum from one matching frame or combine
updates solely because their lengths match another model's implementation.

Maintain a metric ledger:

| Field | Required information |
| --- | --- |
| Raw source | Service/characteristic, message type and field/byte location |
| Interpretation | Endianness, signedness, scale, unit, presence conditions |
| Behavior | Instantaneous/cumulative/status, reset, persistence and wrap rules |
| Provenance | Device reported, device estimate, host derived, app derived, or unknown |
| Evidence | Raw event IDs and experiment observations |
| Confidence | Verified, correlated, hypothesis, or unknown |

Keep a real zero distinct from missing, unavailable, unknown, and stale.
Preserve original device counters even when producing a normalized workout
total. Keep host timestamps independent of any device timer. Do not invent
power from cadence and present it as measured bike power, or assume the app's
calorie/distance values are independently transmitted fields.

Use meaningful tests based on captured frames and concrete risks: field
presence, truncation, unknown messages, fragmentation, count/time wrap,
stop/resume, reset/reconnect, and replay equivalence. Unknown messages and
decode errors must never suppress raw recording. Synthetic fixtures should be
labeled as such; they are not evidence of hardware compatibility.

## 10. Redis and continuous operation

Keep Redis downstream of the recording path. The starter already forwards raw
GATT value updates to `sunny:bike:raw` when SUNNY_REDIS_URL is set. It currently
does not forward the full inventory, advertisements, reads, or lifecycle
events through a unified schema, and it does not reconnect to Redis.

Expand downstream distribution after raw recording is reliable:

- A raw event channel or stream that retains unknown messages.
- A separate decoded telemetry channel.
- A latest decoded state key with event time and freshness/connection status.
- Replay from the local journal, or Redis Streams when retained broker history
  is required, with documented retention and deduplication.

Redis Pub/Sub is at-most-once; a successful PUBLISH does not prove a subscriber
existed or saved the record. Streams retention and server persistence also
require explicit configuration. Keep source event IDs through retries/replay,
and distinguish a confirmed failure from an unacknowledged command that may
have been delivered. [13]

Test a Redis outage and a slow downstream consumer without silently losing
the local capture. Also test writer failure/overflow behavior and graceful
termination. Continuous operation must recover from BLE sleep/disconnect with
fresh discovery/subscriptions and visible gaps. A service manager and the web
backend can follow once the collector's behavior is established.

## 11. Deliverables and acceptance criteria

### Milestone A: evidence and comprehensive raw acquisition

- Working Linux build and an observed connection to the confirmed bike.
- Complete discovered inventory, including explicit discovery/read failures.
- Every advertisement/read/notify/indicate source assigned a coverage status.
- Raw data, unknown payloads, outgoing requests, repeats, lifecycle events, and
  experiment markers preserved under the declared capture plan.
- Descriptor and read-only data investigated and polling needs documented.
- HCI and application captures compared by characteristic/payload and interval;
  missing/duplicate records have explanations or remain explicit gaps.
- The experiment matrix is exercised or its untested scenarios are listed.
- Known local loss and uncertain upstream/disconnect gaps are reported honestly.

### Milestone B: verified protocol and decoding

- A model-specific protocol document, including any verified initialization,
  polling, keepalive, and reassembly rules.
- Every claimed metric has a raw source, units, provenance, and test evidence.
- Stop/resume/reset/reconnect behavior is validated.
- Unknown fields remain preserved and listed; they are not guessed away.
- Offline replay produces the same decoded results from the same raw inputs.

### Milestone C: reliable application integration

- Reconnect/sleep recovery restores the required data paths.
- Raw recording continues independently of decoder and Redis failures within
  the stated storage/queue limits, with explicit failure status beyond them.
- Redis publishing/replay and stale/connection-state reporting are validated.
- README, protocol documentation, coverage report, and validation notes match
  what actually works on the hardware.

Update `VALIDATION.md` with exact commands, source revision, hardware/firmware,
run references, and outcomes. Distinguish implemented, experimentally observed,
and inferred behavior. Do not describe the full protocol as reversed while
known data sources or fields remain unexplained.

The first useful increment is a complete raw evidence set and a source-by-source
coverage report. Then improve acquisition gaps and decoding until every
exposed source found in the tested scenarios is accounted for. Continue
autonomously through ordinary coding and analysis; report actual hardware or
permission blockers with the next concrete action the user needs to perform.

## 12. Source references

Research inspected October 16, 2026. Pinned source links below preserve the
implementations that were examined; neither stepper reference establishes
mini-bike compatibility.

1. [Sunny Smart Mini Exercise Bike product](https://sunnyhealthfitness.com/products/under-desk-smart-mini-exercise-bike) and [SF-B023005DGY assembly/manual page](https://sunnyhealthfitness.com/blogs/assembly-guides/sf-b023005dgy).
2. [Manufacturer's SF-B023005DGY English manual, January 20 2026, version 2.0](https://share.sunnyfit.com/web_static/op/user_manual/SF-B023005DGY_EN_20260120_V2.0.pdf): BLE connectivity, meter fields, idle sleep/wake, and counter reset behavior.
3. [Bluetooth SIG Assigned Numbers](https://www.bluetooth.com/specifications/assigned-numbers/): standard service and characteristic identifiers.
4. [Fitness Machine Service](https://www.bluetooth.com/specifications/specs/fitness-machine-service-1-0/) and [Cycling Speed and Cadence Service](https://www.bluetooth.com/specifications/specs/cycling-speed-and-cadence-service-1-0/).
5. [BlueR 0.17.4 API](https://docs.rs/bluer/0.17.4/bluer/) and [BlueZ Device API](https://bluez.readthedocs.io/en/latest/device-api/).
6. [BlueR notification source](https://docs.rs/bluer/latest/src/bluer/gatt/remote.rs.html) and [remote Characteristic API](https://docs.rs/bluer/0.17.4/bluer/gatt/remote/struct.Characteristic.html). Review the pinned 0.17.4 implementation when addressing startup readiness and notify_io constraints.
7. [BlueZ btmon documentation](https://github.com/bluez/bluez/wiki/btmon) and [Wireshark Bluetooth Attribute Protocol fields](https://www.wireshark.org/docs/dfref/b/btatt.html).
8. [Swift SunnyFit UUID definitions](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/Sources/SunnyFit/Extensions/BluetoothUUID.swift), [notification decoder](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/Sources/SunnyFit/SunnyFitNotification.swift), and [app connection code](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/SunnyFit.swiftpm/Model/AccessoryManagerBluetooth.swift).
9. [QZ SunnyFit stepper implementation](https://github.com/cagnulein/qdomyos-zwift/blob/2e0bd25a4a28a3ebf679a8f8f71551f6473207df/src/devices/sunnyfitstepper/sunnyfitstepper.cpp).
10. [SunnyFit SF-E902 SMART-H advertisement test](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/Tests/SunnyFitTests/EllipticalTests.swift).
11. [Android Bluetooth verification/debugging and HCI logging](https://source.android.com/docs/core/connect/bluetooth/verifying_debugging).
12. [Android bug report capture](https://developer.android.com/studio/debug/bug-report).
13. [Redis Pub/Sub delivery semantics](https://redis.io/docs/latest/develop/pubsub/) and [Redis Rust async API](https://docs.rs/redis/1.7.1/redis/trait.AsyncCommands.html).
