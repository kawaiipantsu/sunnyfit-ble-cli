# sunny-ble

A Linux Rust CLI for discovering an exercise bike's Bluetooth LE interface,
inspecting its GATT services, and recording raw value updates. It can forward
the same JSON events to Redis Pub/Sub.

**Confirmed target:** Sunny Health & Fitness SF-B023005DGY. The user confirmed
the model on October 16, 2026 and wants to collect all Bluetooth data the bike
exposes, including unknown fields and messages.

**Status:** this is a compiled reconnaissance collector, not a verified decoder
or a complete recorder for the bike. Its service inventory, firmware,
initialization sequence, and metric encoding still need to be established on
the Linux host. No SunnyFit UUIDs, packet offsets, or initialization commands
are assumed by the program.

**Continuing with Codex:** read [CODEX_HANDOFF.md](CODEX_HANDOFF.md) first. It
defines the complete-data goal, the starter's current gaps, the host capture
workflow, and the implementation and validation requirements. The comprehensive
recording features described there are requested next steps, not features
already implemented in this source.

## 1. Build and prepare Linux

Use Rust 1.90 or newer. Install Rust using the instructions at
[rust-lang.org/tools/install](https://rust-lang.org/tools/install/).

On Debian/Ubuntu:

```bash
sudo apt-get update
sudo apt-get install bluez libdbus-1-dev pkg-config build-essential
sudo systemctl start bluetooth
bluetoothctl show
bluetoothctl power on

cargo build --release --locked
./target/release/sunny-ble --help
```

For a build without the system D-Bus development package, use:

```bash
cargo build --release --locked --features vendored-dbus
```

The vendored option builds the D-Bus client library. It still needs a C compiler,
and it does not supply a running system D-Bus daemon, BlueZ, or a Bluetooth
adapter. Run the collector on the Linux host with access to its system bus.
Use the distribution's normal Bluetooth permissions; the CLI does not require
being launched as root in an appropriately configured session.

The program leaves adapter power under your control. It reports an error if the
selected adapter is off. Select an adapter explicitly when needed:

```bash
./target/release/sunny-ble --adapter hci0 scan --seconds 20
```

Wake the bike by pedaling or pressing its display button. Disconnect SunnyFit
and other Bluetooth clients while testing the Linux connection; turning off
Bluetooth on the phone is a useful way to remove a competing connection.

## 2. Find the bike

```bash
./target/release/sunny-ble scan --seconds 20 > scan.jsonl
```

Each line is JSON. Look at `address` and `properties.name`, as well as
`properties.service_data_hex`, `properties.manufacturer_data_hex`, and
`properties.service_uuids`. Do not assume the advertised name contains
"Sunny". Compare a scan before and after waking the bike when identification
is ambiguous.

BlueZ may report cached devices. `source: "cached"` and `cached_at_start`
make this explicit. Later discovery/property-change records are not a raw
advertisement trace either. In particular, `service_uuids` can include cached
or resolved service knowledge. A listed address or old RSSI alone does not
prove the device is currently nearby. Identical metadata snapshots are
deduplicated; this command is not a lossless advertisement recorder.

The target model is confirmed as SF-B023005DGY. Record its observed Bluetooth
identity and any readable firmware information without assuming an advertised
name or address from the model number alone.

## 3. Connect and inspect GATT

Replace this example address with the address from your scan:

```bash
./target/release/sunny-ble inspect AA:BB:CC:DD:EE:FF --read > gatt.jsonl
```

This lists every service, characteristic, and descriptor, along with UUIDs and
decimal BlueZ instance IDs. `--read` also reads characteristics that advertise
read support and records their values as `read_hex`. A failed value read is
recorded as `read_error` so the rest of the inventory can continue. Other setup
or inventory errors cause a nonzero exit; check stderr and the exit status.

The optional read pass can reveal model/firmware strings and features. It is
separate from notification capture. The program issues no application command
writes. Subscriptions in the capture command do change the notification
configuration through BlueZ.

### Connecting and pairing are different

Start without `--pair`. A BLE GATT connection does not itself imply bonding.
If the bike requires Bluetooth security for its characteristics, try:

```bash
./target/release/sunny-ble inspect AA:BB:CC:DD:EE:FF --pair --read > gatt-paired.jsonl
```

This explicitly requests pairing/bonding using a NoInputNoOutput agent. It
never marks the device trusted. If a PIN/passkey or confirmation is required,
pair interactively in `bluetoothctl`, then rerun the CLI. A SunnyFit-specific
application handshake is a separate issue from Bluetooth pairing; pairing
alone will not reproduce that handshake.

The setup/read timeout defaults to 20 seconds per operation. For a slow
adapter, use `--timeout-seconds 40`.

### Recognize protocol candidates

These are discovery clues, not a claim about the bike you have:

| Service | Service UUID | Measurement characteristic |
| --- | --- | --- |
| Fitness Machine Service (FTMS) | `0x1826` | Indoor Bike Data: `0x2AD2` |
| Cycling Speed and Cadence | `0x1816` | CSC Measurement: `0x2A5B` |
| Cycling Power | `0x1818` | Cycling Power Measurement: `0x2A63` |
| Proprietary SunnyFit family seen in other machines | `FD710001-E950-458E-8A4D-A1CBC5AA4CCE` | Inspect the actual characteristics |

Standard 16-bit UUIDs appear as full Bluetooth UUIDs, for example
`00001826-0000-1000-8000-00805f9b34fb` and
`00002ad2-0000-1000-8000-00805f9b34fb`.

A service UUID advertised by the bike is a useful lead. Confirm its actual
characteristics and payloads after connecting. Advertising may omit services;
the full connected GATT inventory is the more useful starting point.

Existing SunnyFit source defines these additional proprietary characteristics
with the same `-E950-458E-8A4D-A1CBC5AA4CCE` suffix:

| Prefix | Role in the existing Swift implementation |
| --- | --- |
| `FD710002` | Command |
| `FD710003` | Notification 1 |
| `FD710004` | Notification 2 |
| `FD710005` | Command 2 |
| `FD710006` | Notification 3 |

The known implementations concern other Sunny machines, especially steppers.
Their command sequences and metric parsers must not be assumed to apply to a
mini bike. The CLI discovers UUIDs dynamically and never sends those commands.

## 4. Capture while pedaling

For the first capture, subscribe to all accessible characteristics offering
notify or indicate:

```bash
./target/release/sunny-ble capture AA:BB:CC:DD:EE:FF --seconds 90 \
  > ride.jsonl 2> ride.log
```

The 90-second interval begins after subscription setup. The diagnostic log
contains a `Ready` line. A simple capture sequence after that line is:

| Approximate time | Action | Record alongside the capture |
| --- | --- | --- |
| 0-15 seconds | Keep pedals still | Starting display values |
| 15-35 seconds | Pedal slowly and steadily | Speed, distance, time, calories |
| 35-55 seconds | Increase the pedaling rate | The same display values |
| 55-70 seconds | Stop | Whether values freeze, drop, or continue |
| 70-90 seconds | Resume, or count a known number of full turns | Turn count and final values |

Do separate short captures for display reset and any resistance changes. A
mechanical resistance knob is not evidence of an electronic resistance sensor.
An app may calculate metrics that the bike never sends directly.

Automatic selection is best effort: failures on individual characteristics
are logged, and collection proceeds if at least one subscription succeeds.
Read `ride.log` for skipped characteristics. If no subscription succeeds, the
command fails. To select specific discovered instances instead:

```bash
# Example IDs only: replace them with decimal IDs from your own gatt.jsonl.
./target/release/sunny-ble capture AA:BB:CC:DD:EE:FF \
  --select 12:14 --select 12:17 --seconds 90 \
  > ride-selected.jsonl 2> ride-selected.log
```

An explicitly selected missing, unsupported, or inaccessible characteristic
is an error. IDs are local BlueZ discovery identifiers, not permanent device
identifiers or guaranteed stable ATT handles. Re-inspect after rediscovery.
Both service and characteristic IDs are retained because UUIDs alone may not
uniquely identify an instance.

Omit `--seconds` to capture until Ctrl-C. On Ctrl-C or an error, subscriptions
are dropped. A connection that this process opened from a disconnected state
is disconnected; a pre-existing connection is left connected. Unexpected BLE
disconnection ends the command with an error. This version does not reconnect.

### Raw event fields

Every captured update has:

- `schema`: `sunny-ble.raw.v1`.
- `type`: `gatt_value_update`.
- `session_id`: a new random UUID per capture invocation.
- `sequence`: host ingestion sequence within that capture, starting at 1.
- `timestamp_unix_ms`: host wall-clock processing time in Unix milliseconds.
- `elapsed_ms`: monotonic host time since capture preparation began, including
  GATT discovery and subscription setup.
- `adapter` and `address`.
- `service_id`, `service_uuid`, `characteristic_id`, `characteristic_uuid`.
- `value_hex` and `value_length`: the original bytes of that value update.

These are host ingestion times, not device measurement timestamps or exact
radio arrival times. Streams from different characteristics may be merged in
a different order from the wire. Packet boundaries are retained; the program
does not concatenate notifications or interpret the bytes as text or metrics.

BlueR's normal notification stream exposes BlueZ value changes. Other clients'
reads can also change a cached value. Use a single client while investigating,
and keep the HCI capture below as the source for exact protocol ordering.

### Keep an HCI trace alongside Rust

Start this in another terminal before connecting:

```bash
sudo btmon -i hci0 -w sunny-linux.btsnoop
```

Stop it after the capture. Inspect it with Wireshark, or:

```bash
btmon -r sunny-linux.btsnoop
```

In BlueR 0.17.4, `notify()` calls StartNotify before registering its value-event
receiver. From that source order, an immediate startup update could be missed
by the Rust stream. `btmon` records the Linux controller traffic independently
and is useful for investigating startup and subscription details. It does not
capture a separate phone-to-bike connection over the air.

## 5. Optional Redis forwarding

With a Redis server already running locally, start a subscriber first:

```bash
redis-cli SUBSCRIBE sunny:bike:raw
```

Then capture and publish:

```bash
SUNNY_REDIS_URL=redis://127.0.0.1:6379/ \
  ./target/release/sunny-ble capture AA:BB:CC:DD:EE:FF --seconds 90 \
  > ride.jsonl 2> ride.log
```

Use `--channel sunny:desk-bike:raw` to change the channel. For real credentials,
provide the environment through a private configuration mechanism rather than
typing a credential-bearing URL into shell history. Diagnostic messages do
not print the Redis URL. This build includes the plain local Redis transport;
TLS support is not enabled in the dependency features.

Each JSONL event is written and flushed to stdout before the same JSON string
is queued for Redis. The Redis worker has a bounded 512-message queue, a
five-second connect timeout, a two-second PUBLISH timeout, and a five-second
shutdown allowance. Redis cannot block the Bluetooth loop through this queue.
However, a slow stdout consumer can block collection; redirect to a normal
local file for reconnaissance. Flush is not an fsync durability guarantee.

The worker connects once per invocation. A Redis error disables forwarding
for the remainder of that capture, while JSONL continues. Queue drops and
failed/unconfirmed deliveries are counted and reported. Incomplete or
unavailable forwarding produces a nonzero final status. A timed-out PUBLISH
may have reached Redis even if the acknowledgement was not received; use
`session_id` plus `sequence` for deduplication during any later replay.

Redis Pub/Sub is at-most-once. A successful PUBLISH means Redis acknowledged
the command; it does not prove a subscriber was present or stored the sample.
Use the JSONL file as the capture record. For a later application, add a latest
decoded state key and, if replay/history matters, Redis Streams or a database.
Neither decoded state nor Streams is implemented by this version.

## 6. If notifications are silent

First check `ride.log` for successful subscriptions, errors, and the final
update count. Keep the bike awake, disconnect the phone, and confirm the same
address/model again. No values might mean an idle device, a wrong characteristic,
read/poll-based telemetry, advertisement-based telemetry, required Bluetooth
security, or a missing application initialization exchange. It is not enough
evidence by itself to conclude encryption or incompatibility.

If SunnyFit displays live values while the Rust connection does not, record
a short successful SunnyFit session on your own Android phone:

1. Enable Developer options and **Bluetooth HCI snoop log**.
2. Restart Bluetooth so logging takes effect.
3. Start with the bike disconnected, then connect inside SunnyFit.
4. Record an idle interval, slow pedaling, faster pedaling, and a stop.
5. Generate a bug report using the phone's option or `adb bugreport`.
6. Extract the Bluetooth trace if present, and view it in Wireshark. AOSP also
   documents `btsnooz.py` for an embedded trace in the text report.

Log locations and inclusion in bug reports vary by phone/Android version.
Do not assume `/sdcard/btsnoop_hci.log` is accessible or that a report contains
full payloads. Full HCI logging must be enabled before the relevant session.
When sharing, extract the bike-specific traffic; a complete phone bug report
can contain unrelated personal diagnostics.

Look for service discovery, descriptor subscriptions, characteristic writes,
write type (with or without response), notification/indication replies, and
recurring polls or keepalives. Wireshark's `btatt` display filter is useful once
you have selected the bike's connection. Map handles from that trace to UUIDs;
do not assume handles from the phone are interchangeable with Linux IDs.

Build an initialization transcript from observations: destination UUID,
request bytes, response bytes, and timing. After validating it for the exact
model, a future CLI change can subscribe first, send the confirmed messages,
and wait for the expected responses. This version intentionally has no
command-write interface because there is no confirmed bike transcript yet.

## 7. Decode and extend after the first capture

For standard FTMS or CSC, use the applicable Bluetooth SIG data definitions.
FTMS fields are flag-dependent; do not use a fixed offset across every frame.
CSC typically needs deltas of revolution counters and event times, including
counter wrap handling, to derive cadence. Preserve packet validity and absent
values rather than turning missing fields into zero.

For proprietary data, compare frames from controlled sessions against the
display. Establish framing, lengths, endianness, scale, counters, reset and
wrap behavior, and any integrity fields separately. A notification may be a
fragment or a different record type. Use observations to decide whether bytes
should be combined. Do not infer a checksum algorithm from one matching frame.

Keep the future decoder independent of Bluetooth I/O so recorded captures can
be used as fixtures. Keep acquisition session identity distinct from a workout
session: reconnecting does not necessarily reset the bike's counters. Mark
derived metrics and units explicitly. Unsupported metrics should remain absent
or null. A bike display showing speed/distance/calories does not establish that
all those fields are sent as independent Bluetooth values.

The later server path can be: Rust acquisition and decoder, Redis, a web
backend that subscribes, and a WebSocket to the browser. The browser talks to
the backend. Before running continuously, add reconnect with backoff and fresh
GATT discovery, connection/stale-state events, recording/replay policy, and a
bounded web-client delivery strategy. A temporarily silent stream is not
necessarily a disconnect or a measured zero cadence.

## 8. What to collect for the next decoding step

With SF-B023005DGY confirmed, collect the output of `inspect --read`, a short
`ride.jsonl` and its diagnostic `ride.log`, and notes or a timed video of the
display/actions. Include a bike-specific HCI trace if notifications never
start or the first messages appear to be missing. Address/name metadata can
be redacted consistently; retain UUIDs, payload bytes, and relative timing.

## Sources and research boundary

Checked on October 16, 2026. Protocol evidence is model-specific.

- [Official Smart Mini Exercise Bike product](https://sunnyhealthfitness.com/products/under-desk-smart-mini-exercise-bike): Bluetooth SunnyFit connectivity and display metrics.
- [Official SF-B023005DGY assembly/manual page](https://sunnyhealthfitness.com/blogs/assembly-guides/sf-b023005dgy).
- [Manufacturer's January 2026 manual](https://share.sunnyfit.com/web_static/op/user_manual/SF-B023005DGY_EN_20260120_V2.0.pdf): Bluetooth LE, meter behavior, four-minute idle sleep, and wake behavior.
- [Bluetooth SIG Assigned Numbers](https://www.bluetooth.com/specifications/assigned-numbers/): normative service and characteristic UUID names.
- [Bluetooth SIG Fitness Machine Service](https://www.bluetooth.com/specifications/specs/fitness-machine-service-1-0/).
- [Bluetooth SIG Cycling Speed and Cadence Service](https://www.bluetooth.com/specifications/specs/cycling-speed-and-cadence-service-1-0/).
- [MillerTechnologyPeru/SunnyFit UUID definitions](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/Sources/SunnyFit/Extensions/BluetoothUUID.swift): proprietary-family UUIDs, not verified mini-bike support.
- [SunnyFit captured Air Walk advertisement test](https://github.com/MillerTechnologyPeru/SunnyFit/blob/8806914f8e2a5dc54df8c7dc01171fc449f40e5c/Tests/SunnyFitTests/EllipticalTests.swift): a different Sunny model advertises FTMS service data.
- [QZ SunnyFit stepper implementation](https://github.com/cagnulein/qdomyos-zwift/blob/2e0bd25a4a28a3ebf679a8f8f71551f6473207df/src/devices/sunnyfitstepper/sunnyfitstepper.cpp): corroborates the proprietary service on a different machine. Its parser and the Swift parser are not a verified decoder for this bike.
- [BlueR 0.17.4](https://docs.rs/bluer/0.17.4/bluer/), [remote characteristics](https://docs.rs/bluer/0.17.4/bluer/gatt/remote/struct.Characteristic.html), and [notification implementation](https://docs.rs/bluer/latest/src/bluer/gatt/remote.rs.html).
- [BlueZ Device API](https://bluez.readthedocs.io/en/latest/device-api/) and [btmon](https://github.com/bluez/bluez/wiki/btmon).
- [Android Bluetooth debugging](https://source.android.com/docs/core/connect/bluetooth/verifying_debugging) and [bug reports](https://developer.android.com/studio/debug/bug-report).
- [Redis Pub/Sub delivery semantics](https://redis.io/docs/latest/develop/pubsub/) and [Rust Redis async commands](https://docs.rs/redis/1.7.1/redis/trait.AsyncCommands.html).

## Verification

See `VALIDATION.md` for the checks performed on this version and the hardware
checks that remain. The archive contains source and Cargo.lock, not a
precompiled binary or an assertion that the proprietary protocol is decoded.
