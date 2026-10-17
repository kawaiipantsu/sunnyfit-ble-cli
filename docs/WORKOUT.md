# The terminal workout app

```sh
sunnyfit-ble-cli --workout --goal-minutes 25 --ghost latest --route denmark
sunnyfit-ble-cli --demo --workout --met 3
```

`--workout` automatically shares the local system service's stream when it is
running, displaying **SERVICE STREAM**. Without the service it owns Bluetooth
itself. Attached workouts keep independent timers, distance, training and local
history; pause/quit leave the collector and its storage destinations running.
Service restarts trigger reattachment without a second Bluetooth connection.
See [service access and source selection](SERVICE.md#sharing-the-service-with-a-workout)
for group permissions, `--workout-source` and custom socket paths.

Tab / Shift-Tab switch views; 1–9 jump directly. Space or `p` pauses local totals
and training; `m` marks the capture; `q`, Escape or Ctrl-C saves and exits.
The interface renders on stderr, leaving stdout available for requested JSON.

| View | Features |
| --- | --- |
| Ride | Gradient speed graph, minimum/maximum guides, elapsed/active time, energy and food comparisons |
| Gauges | Speed, cadence target and active-time gauges |
| Intervals | Repeating programmable stages, target cadence bands, countdown and time in target |
| Ghost | Race a previous session's distance at the same active riding time |
| Routes | Simulated Denmark, Route 66 or summit progress, checkpoints and remaining distance |
| Town | Scrolling ASCII town driven by accumulated workout distance |
| Games | Cadence Gates and Cadence Flight; `g` switches games |
| Desk | Pomodoro, movement reminders, cadence alerts and daily active-time goal |
| Progress | Persistent XP, levels, achievements and a date-based daily movement challenge |

Missing measurements remain unavailable. Training and games freeze when paused
or bike telemetry becomes stale/disconnected. Interval recovery stages continue
while the bike reports a fresh zero cadence. The Pomodoro clock is independent of
the bike; Enter pauses it and `r` resets it in Desk view. Desktop notifications
are opt-in with `--notifications` and require `notify-send` / a desktop session.
The same messages are visible in the TUI without a notification daemon.

## Units and speed records

The cards show speed in km/h and mph, cadence in rpm and revolutions per second
(rps), and distance in kilometres, miles and metres. The graph uses a uniform
lower-line `▁` outline over its gradient fill.

The speed table separates the visible **WINDOW**, the entire **RIDE**, and
**ALL-TIME** low/average/max values. The visible window includes fresh zero-speed
samples; ride and all-time statistics cover measured active time. Averages for
rides and lifetime records are weighted by time, not by the number of workouts.

When sharing the service, all-time records come from its selected bike's stored
MySQL or SQLite lifetime row. The collector refreshes this read-only cache about
every five seconds and includes it in the local stream; viewers need no database
credentials. Standalone workouts use database records when configured, otherwise
saved local workouts plus the current ride. Local history includes available
measurements across saved rides; old summaries without speed-time weights or
minimums contribute only the fields actually recorded. Version 0.4 captures can
recover their weights/minima from recorded statistics. Demo workouts never load
or contribute to real all-time records. Unavailable values appear as `—`.

## Intervals

`--interval-plan workouts/desk-intervals.toml` loads a validated TOML plan. Without
a file, the Intervals view offers a 12-minute desk rhythm plan. Targets guide your
cadence; they do not change bike resistance.

```toml
name = "Comfortable desk intervals"
repeats = 4
[[steps]]
name = "Steady"
seconds = 120
cadence_min = 40
cadence_max = 65
[[steps]]
name = "Recovery"
seconds = 60
cadence_min = 25
cadence_max = 45
```

## Ghosts, routes and progress

`--ghost latest` uses the newest completed non-demo capture with distance;
`--ghost best` uses the greatest recorded distance. An explicit run directory or
`events.jsonl` also works. Ghost distance is interpolated against active time and
holds at its finish. It does not extrapolate a faster or longer ride.

`--route denmark`, `route66` or `summit` selects illustrative routes of 430 km,
3,940 km or 12 km. They are simulated using accumulated workout distance, with
no GPS, real geographic navigation, elevation or resistance changes. The bike's
reported counter is preferred; configured/integrated distance estimates retain
their provenance in the saved telemetry. Route progress accumulates across TUI
sessions selecting that route.

`progress.json` stores per-run contributions, merged under a file lock every 30
seconds and on clean exit. Re-saving a run does not award XP twice. XP is ten per
active minute plus 25 per kilometre; levels advance every 500 XP. Daily movement
uses the host's local date. Demo progress is visible but never saved. Headless
connector sessions do not award workout XP.

## Desk settings

```sh
sunnyfit-ble-cli --workout --focus-minutes 25 --break-minutes 5 \
  --movement-reminder-minutes 30 --daily-goal-minutes 30 \
  --cadence-alert-rpm 30 --cadence-alert-seconds 20 --notifications
```

Reminders encourage comfortable movement; they do not prescribe an exercise
intensity. Cadence alerts require a fresh measurement and respect pause/stale
state. Active minutes only accumulate from observed movement.

## A slice of motivation

[`callories-burn.json`](../callories-burn.json) deliberately keeps the requested
filename. It includes 25 food/portion examples with approximate energy values.
First start copies it to `~/.config/sunnyfit-ble-cli/callories-burn.json`; edit that
copy or select another file with `--food-file PATH` / `SUNNY_FOOD_FILE`. Each food
needs a `name`, `portion` and positive `kcal` value inside the `foods` array.

The live Ride view and saved summary can say “Wow, that's almost a slice of
cheese pizza!” Comparisons use device-estimated or explicitly calibrated workout
energy, and are approximate energy equivalents, not food intake measurements.
The file records references and which entries are illustrative. Unavailable
calories produce no food comparison.
