#!/usr/bin/env python3
"""Check the configured APT API or explicitly sign a Debian package.

Private service addresses and credentials stay in the external instructions file.
Readiness checks never upload, stage, publish, or sign a package.
"""
import argparse
import base64
import json
from pathlib import Path
import re
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        # Never forward authentication through an unexpected redirect.
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def fields(text):
    return {
        k.strip(): v.strip().strip('`')
        for k, v in (line.split(':', 1) for line in text.splitlines() if ':' in line)
    }


def instructions(path):
    text = path.read_text()
    signing = fields(text.split('## Signing', 1)[1].split('## APT Deploy', 1)[0])
    apt = fields(text.split('## APT Deploy', 1)[1])
    return signing, apt


def request_json(url, token, payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    headers = {'Authorization': 'Bearer ' + token, 'Accept': 'application/json'}
    if body is not None:
        headers['Content-Type'] = 'application/json'
    req = urllib.request.Request(url, body, headers)
    with OPENER.open(req, timeout=30) as response:
        return json.load(response)


def check(signing, apt):
    print('Private signing credentials:', 'present' if signing.get('API KEY') else 'missing')
    print('Private APT credentials:', 'present' if apt.get('API TOKEN') else 'missing')
    base = apt['API URL'].rstrip('/')
    # This route was verified on the user's exact configured API base. The API
    # root and browser documentation use different scope/authorization rules.
    result = request_json(base + '/suites', apt['API TOKEN'])
    available = result.get('suites')
    if not isinstance(available, list) or not all(isinstance(s, str) for s in available):
        raise ValueError('Unexpected suite response')
    configured = [s for s in re.split(r'[,\s]+', apt.get('SUITES', '')) if s]
    if not configured or any(s not in available for s in configured):
        raise ValueError('Configured suite unavailable')
    print('APT API authenticated successfully; configured suite(s) are available.')
    print('Upload/stage/publish request schema is still required to complete deployment.')
    print('No package signed, uploaded, staged, or published by this check.')


def sign(signing, package, key_id):
    data = package.read_bytes()
    if len(data) > 8 * 1024 * 1024:
        raise ValueError('Signing service limit is 8 MiB')
    guide = urllib.parse.urlsplit(signing['API Guide'])
    if guide.scheme != 'https':
        raise ValueError('Signing requires HTTPS')
    endpoint = urllib.parse.urlunsplit((guide.scheme, guide.netloc, '/api/v1/debian/sign', '', ''))
    result = request_json(endpoint, signing['API KEY'], {
        'key_id': key_id, 'kind': 'deb', 'data_base64': base64.b64encode(data).decode()
    })
    artifacts = result.get('artifacts', [])
    if len(artifacts) != 1:
        raise ValueError('Unexpected signing response')
    # Never trust remote filenames as local paths.
    dest = package.with_suffix(package.suffix + '.asc')
    dest.write_bytes(base64.b64decode(artifacts[0]['data_base64'], validate=True))
    print('Detached signature saved beside the package; no APT upload performed.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--instructions', type=Path, default=Path('/root/xxc-thugs-apt.txt'))
    parser.add_argument('--check', action='store_true', help='Read-only APT authentication/suite check')
    parser.add_argument('--sign', type=Path, help='Create a detached signature for this .deb')
    parser.add_argument('--key-id', help='Signing service OpenPGP key ID')
    args = parser.parse_args()
    if args.sign and not args.key_id:
        parser.error('--sign requires --key-id')
    try:
        signing, apt = instructions(args.instructions)
        if args.check or not args.sign:
            check(signing, apt)
        if args.sign:
            sign(signing, args.sign, args.key_id)
    except urllib.error.HTTPError as error:
        raise SystemExit(f'Service returned HTTP {error.code}; response and credentials withheld')
    except Exception:
        raise SystemExit('Deployment preparation failed; check private instructions, configured suites, input artifact, and service access')


if __name__ == '__main__':
    main()
