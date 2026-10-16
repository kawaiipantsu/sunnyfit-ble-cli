# SF-B023005DGY protocol and coverage

Observed locally on 2026-10-17, using a Realtek RTL8761BU adapter on Debian 13.
Device Information values: firmware `F1.0`, hardware `XT-2366`, software
`V4.0.1_251210`. Addresses and other local identities are intentionally omitted.

## Evidence

The Android package identity `com.amoyyg.sunny` was checked against the official
Google Play listing. Version 2.19.0 (1025) was downloaded from the APKPure mirror
for static inspection, without executing the app or logging into any account.
[Analysis manifest](android-analysis.json) records its hash and relevant classes.
The mirror package's signing identity was not independently verified against a
Google-delivered installation. Protocol interpretations are additionally checked
against actual bike responses and movement/stop captures.

The Android `BleSunnyHorseRidingMachine` class also explicitly handles mini bikes;
its shared class name does not mean stepper offsets should be copied. Its TLV
mapping and the application's unit conversion routines established the units
below. This project contains an independent Rust implementation, not copied app
source. APKs/decompiled files remain under ignored `private/apk/`.

## GATT sources

All proprietary UUIDs below end in `-e950-458e-8a4d-a1cbc5aa4cce`.

| UUID prefix | Role | Observation |
| --- | --- | --- |
| `fd710001` | Sunny service | Found on the actual bike |
| `fd710002` | Query/setup write without response | Permission, info and workout start acknowledged |
| `fd710003` | Query/setup response | Acknowledgements and fragmented device information |
| `fd710004` | Device status notifications | Idle (2) and started (4) seen |
| `fd710005` | Status acknowledgement write | Empty command-4 acknowledgement sent |
| `fd710006` | Motion data notifications | Repeating 39-byte logical frames, split 20 + 19 bytes |

Other services: Generic Access, Generic Attribute, Device Information, and a
vendor service `00010203-0405-0607-0809-0a0b0c0d1912`. Its readable characteristic
and descriptor are inventoried; no speculative firmware writes are sent.

GAP Device Name advertises `notify` but has no CCCD; subscription returned
NotPermitted or stalled. The CLI records that source as unsupported and reads its
value. Service Changed indications use BlueR's fallback notification API. All
three Sunny channels worked through AcquireNotify in the new collector.

## Framing

A frame is `5A command answer_flag payload_length payload checksum A5`.
The checksum is the wrapping byte sum from command through the final payload
byte. Payload length excludes the four-byte header and two-byte trailer. Payload
entries use `length tag value...`, with length including the tag. Numeric values
are little endian. Unknown tags and exact bytes are retained.

The decoder assembles fragments separately for each characteristic instance and
connection epoch, validates length/checksum/trailer, rejects invalid TLVs, limits
buffer size, and expires incomplete assemblies after two seconds. It retains all
original notifications and source-event references. A first fragment alone is
not a complete measurement. Tests exercise every split of a captured motion
frame, malformed frames, unknown tags, gaps and concatenated frames.

## Startup

After subscriptions and an exact model-ID read:

1. Permission request: command 02, TLV A0, the protocol's fixed six-byte value.
2. Query device information: command 01, empty payload.
3. Start reporting: command 02, TLV A3 value **01**, meaning preserve data in the
   app's command builder.
4. Acknowledge status on fd710005 using empty command 04; subsequent status
   reports are acknowledged too.

The first three steps wait for the corresponding complete response. These are
application-protocol messages, distinct from Bluetooth bonding. The tested bike
rejected OS bonding but successfully streamed on an unbonded GATT connection.

The early reference-client experiment used A3 **00** before static analysis
identified its clear-data meaning. That started fresh device session counters;
the odometer persisted. The implementation uses A3 01. Device-side resets/sleep
may still change counters; the collector must not assume continuity just because
it requested preservation. No automatic counter reset, resistance, motor or OTA
commands are implemented.

## Motion TLVs observed on this mini bike

| Tag | Value bytes | Meaning | Scaling/provenance |
| --- | --- | --- | --- |
| 20 | 2 | Speed | unsigned / 100, km/h, device reported |
| 21 | 2 | Cadence | unsigned rpm, device reported |
| 24 | 4 | Session energy | unsigned / 10, kcal, device estimate |
| 25 | 1 | Heart rate | bpm; zero treated as unavailable, not measured zero |
| 26 | 2 | Device session time | seconds |
| 2A | 4 | Device session distance | metres |
| 2B | 4 | Odometer | metres, separate from CLI workout distance |

A steady pedal stage produced 21.89–22.69 km/h and 82–85 rpm. The recorded
movement stage ended at 213 metres, 44 seconds, 4.1 kcal and an 18,267-metre
odometer. Speed/cadence returned to zero after stopping. The user independently
reported approximately 22 km/h during an earlier stage; precise simultaneous
comparison for every display field remains to be completed.

Status TLV F1 is preserved with its numeric value and known enum label. Device
information strings, capability bits, and unknown data remain in decoded TLVs
and raw packets; not every capability bit is assigned a semantic name.

## Scope and remaining experiments

Implemented acquisition covers the discovered inventory, one-time readable
characteristics/descriptors, all usable notification/indication subscriptions,
application writes/responses, reconnect epochs and gaps. Local persistence is
independent of Redis and stdout within its bounded queue/disk limits.

The movement/stop and startup exchanges are observed. Longer sleep/wake cycles,
resistance changes, display MODE changes, deliberate device resets, full official
app sessions and exact display/cadence comparisons remain to be exercised.
Power, electronic resistance and battery are not supplied by the observed
telemetry; absent measurements are not fabricated.

Raw controller advertisements and exact ATT ordering require btmon evidence.
BlueZ property updates cannot establish receipt of every radio transmission.
A complete run means completion of its declared host-observable scope, not proof
that hidden firmware behavior or every radio packet has been discovered.
