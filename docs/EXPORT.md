# Indoor cycling exports and Strava

```sh
sunnyfit-ble-cli export /path/to/run --format csv --output ride.csv
sunnyfit-ble-cli export /path/to/events.jsonl --format tcx --output ride.tcx
sunnyfit-ble-cli export /path/to/run --format fit --output ride.fit
sunnyfit-ble-cli export --database mysql --session UUID --format fit --output ride.fit
```

Exports use the periodic workout snapshots rather than treating every raw packet
as another point. They include elapsed time, available distance, cadence, speed,
heart rate and power. Missing HR/power stay empty/absent. FIT sets cycling and
indoor-cycling sport codes; TCX uses Biking with indoor notes; CSV includes
`activity_type=indoor_cycling`. No GPS or simulated-route coordinates are invented.
TCX requires lap calories/distance: unknown values use zero there with an explicit
note; optional trackpoint measurements remain absent. Calories can be estimates.
FIT and CSV preserve unknown optional values without zero placeholders.
Files are written privately and atomically. The output parent directory must exist.

## Optional Strava authorization

Create your own Strava API application with an appropriate callback domain.
Set `SUNNY_STRAVA_CLIENT_ID`, then run:

```sh
sunnyfit-ble-cli strava-auth
```

Open the printed URL and authorize `activity:write`. Set `SUNNY_STRAVA_CODE`
from the browser redirect and `SUNNY_STRAVA_CLIENT_SECRET` privately, then:

```sh
sunnyfit-ble-cli strava-auth --exchange
sunnyfit-ble-cli sync /path/to/run --name 'Desk cycling'
```

The redirect defaults to `http://localhost`; `SUNNY_STRAVA_REDIRECT_URI` can
change it to your app's registered callback. The CLI does not run a callback web
server. Tokens are stored mode 0600 in `strava-tokens.json` beside the selected
config. Keep client ID/secret available for automatic refresh, or provide a
short-lived `SUNNY_STRAVA_ACCESS_TOKEN` yourself. Never paste these into issues.

`sync` explicitly uploads a FIT file as a trainer/indoor ride. Nothing uploads
when a workout ends or when the system service starts. Private upload receipts
prevent resending a session while Strava processes it; repeat the same command
to poll. If submission has an uncertain outcome, the receipt blocks automatic
retry so you can check your account before removing it. Uploads that Strava
rejects retain their receipt for diagnosis; server response bodies are redacted.

The request/error handling is tested locally. Live Strava OAuth and uploads
require an account/application and have not been exercised for this release.
References: [Strava OAuth](https://developers.strava.com/docs/authentication/),
[Strava uploads](https://developers.strava.com/docs/uploads/),
[Garmin FIT SDK](https://github.com/garmin/fit-python-sdk).
