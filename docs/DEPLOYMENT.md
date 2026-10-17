# Debian packages and APT deployment

Debian packages are distributed through [apt.thugs.red](https://apt.thugs.red).
GitHub hosts the source and issue tracker; GitHub Releases are not part of the
distribution workflow. Follow the [APT setup guide](https://apt.thugs.red/help)
to configure the repository, then install `sunnyfit-ble-cli` through APT.

## Build a package

`make deb` builds `dist/sunnyfit-ble-cli_0.4.1-1_<architecture>.deb` and a SHA-256
checksum. The package installs the executable, documentation, license, and an
system service, example configuration, log rotation and legacy optional user unit.
Installation does not enable or start the service. See [service setup](SERVICE.md).
Runtime requirements are resolved from the built ELF by `dpkg-shlibdeps`; the
Debian 13 build requires libc6 >= 2.39. Build on the oldest supported Debian
release when preparing a package for older systems.

Package metadata uses a generic maintainer. `DEB_MAINTAINER` can be supplied
explicitly for a public release. Git commits use the configured global identity;
private signing and deployment credentials are never package metadata.

## Operator configuration

Private API addresses and credentials are loaded at runtime from the external
`/root/xxc-thugs-apt.txt` runbook. `/root/xxc-thugsred-apt.txt` is an alias on the
operator's host. Other installations can supply `--instructions PATH`.
Neither the file nor its secrets belong in Git, package files or logs.

```sh
make deploy
# Equivalent read-only readiness check:
python3 scripts/deploy.py --check --instructions /path/to/private-runbook.txt
```

The current helper checks authentication and configured suites. It does not
upload, stage or publish; `make deploy` also builds the local package first.
For publishing, use the documented API workflow or an operator-maintained client.
The complete request schemas are available in the
[APT automation guide](https://apt.thugs.red/api/guide.md) and
[OpenAPI reference](https://apt.thugs.red/api/openapi.json).

Use the operator-provided administrative API base exactly. Documentation URLs,
HTML login pages and the bare API root have different routing/authorization.
A root-path `token_scope_required` response does not establish that supported
endpoints are unavailable. Preflight an allowed `/suites` or `/status` request,
including an explicitly authorized `suite` query parameter.

## Publish a reviewed package

All paths below are relative to the configured administrative API base and use
`?suite=<allowed-suite>`. Send `Authorization: Bearer` from a protected credential
source, without cookies. The token needs `read`, `upload`, `stage` and `publish`.

1. POST raw `.deb` bytes to `/uploads`, using
   `Content-Type: application/vnd.debian.binary-package`. Save the returned `id`.
2. POST `/uploads/{id}/stage` with no request body.
3. GET `/repository/diff`. Check its suite and every selected change. Reject
   unrelated additions, unexpected removals, downgrades or removed architectures.
4. POST `/repository/publish` with JSON `{"review_token":"<diff.token>"}`.
5. Poll `/jobs/{job_id}` with the same token and suite until `state` is `succeeded`
   or `failed`. HTTP 202 only acknowledges the job; it is not publication success.
6. Verify the exact package/version through APT and compare its downloaded
   SHA-256 with the original artifact.

A suite is shared: publication can include another project's staged packages.
A changed review requires fetching and inspecting a new diff. After an ambiguous
publish timeout, inspect existing jobs before retrying. Identical uploads are
idempotent; different bytes for the same name/version/architecture require a new
Debian version. Preserve the exact artifact through distribution and verification.

## Signing responsibilities

The APT server signs repository Release/InRelease metadata during publication.
A separate signature on each `.deb` is not required for that workflow.

For an explicitly requested standalone detached signature, the helper also offers:

```sh
python3 scripts/deploy.py --sign dist/sunnyfit-ble-cli_0.4.1-1_amd64.deb \
  --key-id YOUR_SIGNING_KEY_ID
```

This follows the signing service's `POST /api/v1/debian/sign` contract: `key_id`,
`kind: deb`, and base64 file content. It returns a detached `.deb.asc`, preserves
the package bytes and has an 8 MiB input limit. It does not create an embedded
dpkg-sig/debsigs signature or publish to APT. The helper's remote signing action
has not been tested with the operator's key.
