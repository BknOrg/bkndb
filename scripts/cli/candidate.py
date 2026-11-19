#!/usr/bin/env python3
"""Build evidence for CLI candidates. Never publishes or creates Git tags."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
           'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-gnu')


def fingerprint():
    files = [ROOT / p for p in ('Cargo.toml', 'Cargo.lock', '.github/workflows/cli.yml', '.github/workflows/lsm.yml')]
    files += [p for p in (ROOT / 'crates').rglob('*') if p.suffix in ('.rs', '.toml', '.udl', '.h', '.c')]
    files += [p for p in (ROOT / 'fuzz').rglob('*')
              if p.is_file() and 'target' not in p.relative_to(ROOT / 'fuzz').parts
              and (p.suffix in ('.rs', '.toml', '.lock') or 'corpus' in p.parts)]
    files += list((ROOT / 'scripts/cli').glob('*.py'))
    h = hashlib.sha256()
    # WindowsPath ordering folds case; use the exact same POSIX path ordering
    # on every runner so five-target evidence can share one fingerprint.
    for p in sorted(set(files), key=lambda item: item.relative_to(ROOT).as_posix()):
        h.update(p.relative_to(ROOT).as_posix().encode() + b'\0')
        h.update(p.read_bytes().replace(b'\r\n', b'\n') + b'\0')
    return h.hexdigest()


def version():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']


def identity():
    return {'schema_version': 1, 'version': version(), 'source_fingerprint': fingerprint()}


def run(command, **kwargs):
    return subprocess.run([str(v) for v in command], check=True, **kwargs)


def smoke(binary):
    """The copied executable runs with no build tools in PATH or repository cwd."""
    binary = Path(binary).resolve()
    with tempfile.TemporaryDirectory(prefix='bkndb-cli-consumer-') as tmp:
        tmp = Path(tmp)
        executable = tmp / binary.name
        shutil.copy2(binary, executable)
        empty = tmp / 'empty-path'
        empty.mkdir()
        env = os.environ.copy()
        env['PATH'] = str(empty)
        for key in ('RUSTUP_HOME', 'CARGO_HOME', 'RUSTFLAGS', 'LD_LIBRARY_PATH', 'DYLD_LIBRARY_PATH'):
            env.pop(key, None)

        def cli(*args):
            result = run([executable, '--format', 'json', *args], cwd=tmp, env=env, capture_output=True, text=True)
            return json.loads(result.stdout)['data']

        for backend in ('redb', 'lsm'):
            database = tmp / backend
            cli('create', database, '--backend', backend)
            cli('sql', database, '--write', '--query', "CREATE TABLE docs (id INT PRIMARY KEY, text TEXT, embedding LIST); INSERT INTO docs VALUES (1,'native runtime',[1.0,0.0]);")
            assert cli('sql', database, '--query', 'SELECT * FROM docs')['rows'][0][0]['Int'] == 1
            cli('graph', 'node', 'add', database, 'App', '--write')
            cli('graph', 'node', 'add', database, 'App', '--write')
            cli('graph', 'edge', 'add', database, '1', 'LINK', '2', '--write')
            assert cli('graph', 'edge', 'get', database, '1')['edge']['to'] == 2
            cli('index', 'create', database, 'text', 'docs', 'text', '--write')
            cli('index', 'create', database, 'vector', 'docs', 'embedding', '--write')
            assert len(cli('search', 'text', database, 'docs', 'text', 'runtime')) == 1
            assert len(cli('search', 'vector', database, 'docs', 'embedding', '[1.0,0.0]')) == 1
            cli('check', database)
            backup = tmp / (backend + '.backup')
            cli('backup', database, '--output', backup)
            cli('check', backup)


def imports(binary, target):
    if 'linux' in target:
        text = run(['readelf', '-d', binary], capture_output=True, text=True).stdout
        libraries = re.findall(r'Shared library: \[([^]]+)\]', text)
    elif 'apple' in target:
        text = run(['otool', '-L', binary], capture_output=True, text=True).stdout
        libraries = [line.strip().split(' (')[0] for line in text.splitlines()[1:]]
    else:
        text = run(['objdump', '-p', binary], capture_output=True, text=True).stdout
        libraries = re.findall(r'DLL Name:\s*(\S+)', text)
    if any('bkndb' in name.lower() for name in libraries):
        raise ValueError('unexpected BknDb shared library dependency')
    return libraries


def prepare(target, output):
    if target not in TARGETS:
        raise ValueError('unsupported candidate target')
    if os.environ.get('RUSTFLAGS') or os.environ.get('CARGO_ENCODED_RUSTFLAGS'):
        raise ValueError('candidate builds require empty custom Rust flags')
    started = identity()
    run(['cargo', 'test', '--locked', '--release', '--target', target, '-p', 'bkndb-cli', '-p', 'bkndb-engine', '--features', 'bkndb-cli/test-failpoints,bkndb-engine/test-io'], cwd=ROOT)
    # Rebuild WITHOUT failpoints before packaging a candidate executable.
    run(['cargo', 'build', '--locked', '--release', '--target', target, '-p', 'bkndb-cli'], cwd=ROOT)
    suffix = '.exe' if 'windows' in target else ''
    built = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / target / 'release' / ('bkndb' + suffix)
    smoke(built)
    if os.name == 'posix':
        run([os.sys.executable, ROOT / 'scripts/cli/pty_smoke.py', '--binary', built], cwd=ROOT)
    with tempfile.TemporaryDirectory(prefix='bkndb-cli-million-') as tmp:
        run([built, 'stress', Path(tmp) / 'stress', '--operations', '1000000', '--seed', '29', '--format', 'json'], capture_output=True)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    packaged = output / ('bkndb' + suffix)
    shutil.copy2(built, packaged)
    if identity() != started:
        raise ValueError('source changed during candidate preparation; rerun all checks')
    report = started | {'target': target, 'binary': packaged.name,
        'sha256': hashlib.sha256(packaged.read_bytes()).hexdigest(),
        'rustc': run(['rustc', '-Vv'], capture_output=True, text=True).stdout.strip(),
        'native_tests': True, 'runtime_smoke': True, 'million_operations': True,
        'dependencies': imports(packaged, target), 'stable': False}
    (output / 'manifest.json').write_text(json.dumps(report, indent=2) + '\n')


def soak(binary, output, hours):
    if not math.isfinite(hours):
        raise ValueError('soak duration must be finite')
    if hours < 8:
        raise ValueError('acceptance soak must run at least eight continuous hours')
    binary = Path(binary).resolve()
    started = identity()
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    start = time.monotonic()
    seed = 29
    with tempfile.TemporaryDirectory(prefix='bkndb-cli-soak-') as tmp, (output / 'iterations.jsonl').open('w') as log:
        while time.monotonic() - start < hours * 3600:
            database = Path(tmp) / str(seed)
            result = run([binary, 'stress', database, '--operations', '100000', '--seed', str(seed), '--format', 'json'], capture_output=True, text=True)
            log.write(result.stdout)
            log.flush()
            database.unlink()
            Path(str(database) + '.lock').unlink(missing_ok=True)
            seed += 1
    if identity() != started or hashlib.sha256(binary.read_bytes()).hexdigest() != binary_hash:
        raise ValueError('source or executable changed during soak; evidence is invalid')
    report = started | {'passed': True, 'binary_sha256': binary_hash,
        'continuous_seconds': time.monotonic() - start, 'iterations': seed - 29}
    (output / 'soak.json').write_text(json.dumps(report, indent=2, allow_nan=False) + '\n')


def validate(directory):
    """Fail closed unless all runtime, engine, fuzz, soak and review evidence exists."""
    directory = Path(directory)
    current = identity()

    def evidence(path):
        def reject_constant(value):
            raise ValueError(f'{path}: nonfinite JSON value {value}')
        value = json.loads(path.read_text(), parse_constant=reject_constant)
        if not isinstance(value, dict):
            raise ValueError(f'{path}: evidence must be an object')
        for key, expected in current.items():
            if value.get(key) != expected:
                raise ValueError(f'{path}: stale {key}')
        return value

    binary_hashes = set()
    for target in TARGETS:
        manifest = evidence(directory / target / 'manifest.json')
        if manifest.get('target') != target or not all(manifest.get(k) is True for k in ('native_tests', 'runtime_smoke', 'million_operations')):
            raise ValueError(f'{target}: incomplete runtime evidence')
        binary = directory / target / manifest['binary']
        expected_name = 'bkndb.exe' if 'windows' in target else 'bkndb'
        if binary.name != expected_name or binary.parent != directory / target:
            raise ValueError('invalid binary path')
        if hashlib.sha256(binary.read_bytes()).hexdigest() != manifest['sha256']:
            raise ValueError('binary checksum mismatch')
        binary_hashes.add(manifest['sha256'])
        if not manifest.get('rustc', '').startswith('rustc 1.99.1 '):
            raise ValueError('candidate toolchain differs from pinned CI toolchain')
    soak_report = evidence(directory / 'soak.json')
    duration = soak_report.get('continuous_seconds', 0)
    if (soak_report.get('passed') is not True or type(duration) not in (float, int)
            or not math.isfinite(duration) or duration < 28800
            or type(soak_report.get('iterations')) is not int or soak_report['iterations'] < 1):
        raise ValueError('eight-hour soak gate missing')
    if soak_report.get('binary_sha256') not in binary_hashes:
        raise ValueError('soak executable does not match any tested candidate')
    for name in ('engine', 'fuzz', 'audit'):
        report = evidence(directory / (name + '.json'))
        if report.get('passed') is not True:
            raise ValueError(f'{name} gate missing')
    audit = evidence(directory / 'audit.json')
    if audit.get('critical_findings') != [] or not audit.get('reviewer') or not audit.get('scope'):
        raise ValueError('reviewed audit with no unresolved critical findings is required')
    print('Candidate evidence complete; publication remains a separate action.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    prepare_args = commands.add_parser('prepare')
    prepare_args.add_argument('--target', required=True, choices=TARGETS)
    prepare_args.add_argument('--output', required=True)
    smoke_args = commands.add_parser('smoke')
    smoke_args.add_argument('--binary', required=True)
    soak_args = commands.add_parser('soak')
    soak_args.add_argument('--binary', required=True)
    soak_args.add_argument('--output', required=True)
    soak_args.add_argument('--hours', type=float, default=8)
    validate_args = commands.add_parser('validate')
    validate_args.add_argument('--directory', required=True)
    args = parser.parse_args()
    if args.command == 'prepare':
        prepare(args.target, args.output)
    elif args.command == 'smoke':
        smoke(args.binary)
        print('Isolated runtime smoke: OK')
    elif args.command == 'soak':
        soak(args.binary, args.output, args.hours)
    else:
        validate(args.directory)


if __name__ == '__main__':
    main()
