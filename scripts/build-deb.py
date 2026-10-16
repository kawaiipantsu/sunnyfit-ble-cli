#!/usr/bin/env python3
"""Package without copying private files or disclosing the global Git identity."""
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

root = Path(__file__).resolve().parent.parent
os.chdir(root)
pkg = tomllib.loads(Path('Cargo.toml').read_text())['package']
arch = subprocess.check_output(['dpkg', '--print-architecture'], text=True).strip()
version = pkg['version'] + '-1'
stage = root / 'dist' / 'stage'
if stage.exists():
    shutil.rmtree(stage)
(stage / 'DEBIAN').mkdir(parents=True)

def install(src, dest, mode=0o644):
    p = stage / dest
    p.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, p)
    p.chmod(mode)

install('target/release/sunnyfit-ble-cli', 'usr/bin/sunnyfit-ble-cli', 0o755)
for f in ['README.md', 'LICENSE', 'VALIDATION.md', 'CONTRIBUTING.md',
          'docs/PROTOCOL.md', 'docs/USAGE.md', 'docs/DEPLOYMENT.md',
          'docs/android-analysis.json', 'assets/sunnyfit_banner.png',
          'assets/screenshots/workout-demo.png']:
    install(f, 'usr/share/doc/sunnyfit-ble-cli/' + f)
install('packaging/sunnyfit-ble-cli.service', 'usr/lib/systemd/user/sunnyfit-ble-cli.service')
# Resolve required runtime libraries from the actual binary.
shlib = root / 'dist' / 'shlibdeps'
(shlib / 'debian').mkdir(parents=True, exist_ok=True)
(shlib / 'debian/control').write_text('Source: sunnyfit-ble-cli\nSection: utils\nPriority: optional\nMaintainer: SunnyFit maintainers <noreply@example.invalid>\n\nPackage: sunnyfit-ble-cli\nArchitecture: any\nDescription: BLE workout recorder\n')
res = subprocess.run(['dpkg-shlibdeps', '--ignore-missing-info', '-O',
                      '-e' + str(root / 'target/release/sunnyfit-ble-cli')],
                     cwd=shlib, capture_output=True, text=True)
if res.returncode:
    raise SystemExit('Runtime dependency resolution failed; install dpkg-dev and development libraries')
deps = next(line.split('=',1)[1] for line in res.stdout.splitlines() if line.startswith('shlibs:Depends='))
# Public package metadata must not disclose the user's Git identity.
maintainer = os.environ.get('DEB_MAINTAINER', 'SunnyFit BLE CLI maintainers <noreply@example.invalid>')
if '\n' in maintainer or '\r' in maintainer:
    raise SystemExit('Invalid Debian maintainer metadata')
control = f'''Package: sunnyfit-ble-cli
Version: {version}
Section: utils
Priority: optional
Architecture: {arch}
Maintainer: {maintainer}
Depends: bluez, {deps}
Recommends: firmware-realtek
Description: Bluetooth workout recorder and terminal dashboard
 Records SunnyFit BLE data, manages workouts, and publishes to Redis.
Includes unknown raw payloads and offline replay for protocol investigation.
'''.replace('\nIncludes', '\n Includes')
(stage / 'DEBIAN/control').write_text(control)
(stage / 'DEBIAN/control').chmod(0o644)
artifact = root / 'dist' / f'sunnyfit-ble-cli_{version}_{arch}.deb'
subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(stage), str(artifact)], check=True)
import hashlib
artifact.with_suffix('.deb.sha256').write_text(hashlib.sha256(artifact.read_bytes()).hexdigest()+'  '+artifact.name+'\n')
print('Debian package built. Personal Git identity is excluded from package metadata.')
