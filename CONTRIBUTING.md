# Contributing

Start with the [README](README.md), [CLI reference](docs/USAGE.md) and
[protocol notes](docs/PROTOCOL.md). Use the
[issue chooser](https://github.com/kawaiipantsu/sunnyfit-ble-cli/issues/new/choose)
to report a bug, propose a feature, share bike compatibility, or improve the docs.

## Useful reports

For bugs, include `sunnyfit-ble-cli --version`, your distro, the command used,
expected and actual behavior, and a short sanitized log excerpt. For Bluetooth
issues, add the adapter chipset, exact bike model, firmware if known, and whether
the phone was disconnected. Mention whether `--demo` reproduces the problem.

BLE addresses, serial numbers, personal workout data, Redis URLs and credentials
can appear in captures. Redact them before posting; a small relevant excerpt is
usually enough. Do not attach a complete config or recording without reviewing it.
Do not upload APKs, decompiled vendor source or private deployment instructions.

Feature requests should describe the workout or integration problem and what a
successful result would look like. Compatibility reports should separate observed
readings from assumptions and mention which connection flows were exercised.

## Development

Install the prerequisites listed in the README, then use:

```sh
make build
make check
make demo
make deb
```

Use your configured Git identity. Keep changes focused and explain the resulting
behavior and validation in the pull request. Protocol changes should include a
small sanitized fixture, its provenance, and meaningful decoder tests. Distinguish
simulated, replayed and live hardware evidence. Preserve unknown payloads and
measurement provenance; never infer unobserved metrics or counter continuity.

Changes to reconnect, persistence or Redis behavior need relevant failure-path
checks. Documentation and image changes can be checked through their rendered
output and links without rerunning unrelated hardware tests.

## Refreshing the TUI screenshot

The README image is a terminal frame captured from the real demo process. To
regenerate it without touching Bluetooth, Redis, your config or saved workouts:

```sh
sudo apt install python3-pyte python3-pil fonts-dejavu-core
make build
python3 scripts/capture-tui.py
```

The helper captures a 124 × 30 terminal for 36 seconds, rasterizes its actual ANSI
cells and saves `assets/screenshots/workout-demo.png`. It uses an isolated temporary
configuration and a clearly labelled demo session with a one-minute goal.

## Distribution

GitHub hosts source and issue tracking. Debian packages are distributed through
[apt.thugs.red](https://apt.thugs.red); this repository does not require GitHub
Releases or a release-upload workflow. Follow the operator's private APT runbook
when publishing. Never commit private service addresses, tokens or signing material.
