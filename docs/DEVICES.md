# Device profiles and heart-rate sensors

## External heart rate

The connector subscribes to Bluetooth Heart Rate Service **0x180D**, measurement
**0x2A37**, independently of the bike. It supports 8-bit and 16-bit BPM, sensor
contact status, optional energy in kJ and RR intervals in seconds. Contact loss,
zero BPM, stale readings and disconnects clear the displayed rate. Sensor energy
is retained as sensor metadata; it is not added to bike calories. An external
sensor takes precedence over an embedded bike heart-rate value.

```sh
sunnyfit-ble-cli scan
sunnyfit-ble-cli --adapter hci1 wizard AA:BB:CC:DD:EE:00
# The wizard recognizes an HRS-only sensor and saves it separately from the bike.
sunnyfit-ble-cli --workout --heart-rate-device AA:BB:CC:DD:EE:00
# Optional separate adapter:
sunnyfit-ble-cli --heart-rate-adapter hci0 --heart-rate-device AA:BB:CC:DD:EE:00 capture
```

The same settings work in service mode through `SUNNY_HEART_RATE_DEVICE` and
`SUNNY_HEART_RATE_ADAPTER`. Both devices reconnect independently. Wear/wake the
sensor and free a Bluetooth connection slot by disconnecting other applications.
If a model requires OS bonding, perform it with `bluetoothctl` first.

These model families advertise Bluetooth heart-rate compatibility in their
manufacturer documentation; support here is implemented through the shared HRS
protocol, **not proprietary vendor APIs**:

| Manufacturer / examples | Manufacturer reference | Validation here |
| --- | --- | --- |
| Polar H9 / H10 | [Polar standardized heart-rate service guidance](https://support.polar.com/ca-en/troubleshooting-polar-h9-h10-heart-rate-sensor) | HRS fixtures; no physical sensor |
| Garmin HRM-Dual | [Garmin Bluetooth pairing manual](https://www8.garmin.com/manuals/webhelp/hrm-dual/EN-GB/GUID-26E43680-74CC-4CC7-946F-D5019AF632E4.html) | HRS fixtures; no physical sensor |
| Magene H303 | [Magene product compatibility](https://www.magene.com/en/sensors/52-h303-heart-rate-monitor.html) | HRS fixtures; no physical sensor |
| iGPSPORT HR40 | [iGPSPORT manual](https://global.igpsport.com/File/Manual/Pdf/2018/12/29/88c5696446fc.pdf) | HRS fixtures; no physical sensor |

Other sensors should work if they expose the standard Bluetooth HRS measurement.
This does not promise every model from those brands, watch broadcasting modes,
ANT+-only straps, encrypted proprietary features, ECG or vendor offline memory.
No physical heart-rate sensor was available for this release. The test packets
are synthetic standards fixtures, not captures attributed to these manufacturers.
See the [Bluetooth HRS specification](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/HRS_v1.0/out/en/index-en.html).

## Profiles and discovery wizard

```sh
sunnyfit-ble-cli profiles
sunnyfit-ble-cli wizard
sunnyfit-ble-cli --profile ftms capture
sunnyfit-ble-cli --profile csc --wheel-circumference-m 2.1 capture
sunnyfit-ble-cli --profile-file profiles/example.toml capture
sunnyfit-ble-cli validate-profile fixtures/heart-rate.json
sunnyfit-ble-cli validate-profile fixtures/standard-bike.json
```

The wizard scans, asks for a device number, inventories readable GATT services,
characteristics and descriptors, captures the inspection and saves its suggested
profile. For scripting, supply an explicit address. Inspection performs no
application control writes or forced pairing. Unknown devices are saved with
`raw`; their frames remain available for decoder work. The known Sunny startup
is enabled only for the exact model and the auto/Sunny profiles. FTMS and CSC
profiles decode available standard telemetry; devices requiring proprietary
startup or FTMS control negotiation may need additional development.

Declarative profile files map a characteristic UUID to a named metric, offset,
width (1, 2 or 4 bytes), sign, byte order and positive scale. They are read-only
and cannot issue arbitrary startup commands. See `profiles/example.toml`.
Use Rust decoders for variable-length formats, counters, checksums or fragmentation.

Contributions should include a model/firmware description, a sanitized GATT
inventory, a profile and a JSON fixture. Fixture cases have `name` and `frames`;
each frame has `uuid`, `hex`, optional `elapsed_ms`, and either an `expected`
subset of Sample fields or `error: true`. Run `validate-profile` with the same
`--profile` / `--profile-file` selection used for capture, and run `make check`.
Never publish raw identifiers, account data, keys or credentials with fixtures.
