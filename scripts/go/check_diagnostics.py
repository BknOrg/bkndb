#!/usr/bin/env python3
"""Ensure unsupported build configurations fail clearly before native linking."""
import os
from pathlib import Path
import subprocess

module = Path(__file__).resolve().parents[2] / 'bindings/go/bkndb'
for settings in (
    {'CGO_ENABLED': '0'},
    {'CGO_ENABLED': '1', 'GOOS': 'freebsd', 'GOARCH': 'amd64'},
    {'CGO_ENABLED': '1', 'GOOS': 'linux', 'GOARCH': '386'},
    {'CGO_ENABLED': '1', 'GOOS': 'android', 'GOARCH': 'arm64'},
    {'CGO_ENABLED': '1', 'GOOS': 'ios', 'GOARCH': 'arm64'},
):
    env = os.environ.copy()
    env.pop('GOOS', None)
    env.pop('GOARCH', None)
    env.update(settings)
    env['GOWORK'] = 'off'
    result = subprocess.run(['go', 'build', '.'], cwd=module, env=env,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(settings, result.stdout.strip())
    if result.returncode == 0 or 'BknDb_requires_CGO_ENABLED_1' not in result.stdout:
        raise SystemExit('Expected the explicit CGO/platform diagnostic')
print('Build diagnostics: OK')
