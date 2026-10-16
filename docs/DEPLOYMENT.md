# Debian package and private APT deployment

`make deb` builds `dist/sunnyfit-ble-cli_0.2.0-1_<architecture>.deb` and its SHA-256
checksum. The package installs the executable, README/protocol/license, and an
optional systemd user unit. Installation never starts a workout or enables a
service. Runtime library requirements are resolved from the built ELF by
`dpkg-shlibdeps`; the package built on Debian 13 requires libc6 >= 2.39. Rebuild
on the oldest intended Debian release if distributing to older systems.

The package uses generic public maintainer metadata. It deliberately excludes
personal Git identity. `DEB_MAINTAINER` can be supplied explicitly when preparing
an intentionally public release. Git commits themselves use the existing global
identity, with no repository-local replacement and no configured remote.

Private service URLs and credentials are read at runtime from the external
`/root/xxc-thugs-apt.txt`, or a different file passed with `--instructions`.
They are not copied into source, package contents, command arguments or logs.

```sh
make deploy
python3 scripts/deploy.py --sign dist/sunnyfit-ble-cli_0.2.0-1_amd64.deb \
  --key-id YOUR_SIGNING_KEY_ID
```

The signing helper follows the signing service's inspected OpenAPI:
`POST /api/v1/debian/sign`, bearer authentication, `key_id`, `kind: deb`, and
base64 file content. The service returns a detached `.deb.asc` signature, not
an embedded dpkg/debsigs signature. Its documented file limit is 8 MiB. Signing
is explicitly invoked and has not been exercised with the user's key.

The supplied **API URL** is used directly for API operations. `--check` performs
an authenticated `GET <API URL>/suites` and verifies that the configured suites
exist. This succeeded against the user's server. The API root itself returned
`token_scope_required`; that response does not describe access to the supported
suite endpoint or prove that upload/publish operations are unavailable.

The separate API Guide URL returned a ban page. Accessing `/api` on the local API
server instead redirected to the browser admin login; a bearer token did not
grant that documentation route. These documentation failures must not be
reported as an APT API outage. The token's private instructions list read, upload,
stage and publish scopes, but their write request schemas still need to be read
before implementing those actions. No guessed write request has been sent.

No package has been signed, uploaded, staged or published. Repository
Release/InRelease signing is separate from a detached package signature and must
follow the APT server's actual publication workflow. The local package is built,
installed and tested while the deployment request schema is being obtained.
