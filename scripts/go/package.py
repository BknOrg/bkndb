#!/usr/bin/env python3
"""Build, verify and exercise the self-contained Go distribution (Python 3.9+)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
MODULE = ROOT / 'bindings/go/bkndb'
MODULE_PATH = 'github.com/BknOrg/bkndb/bindings/go/bkndb'
TARGETS = {
    'linux_amd64': 'x86_64-unknown-linux-gnu',
    'linux_arm64': 'aarch64-unknown-linux-gnu',
    'darwin_amd64': 'x86_64-apple-darwin',
    'darwin_arm64': 'aarch64-apple-darwin',
    'windows_amd64': 'x86_64-pc-windows-gnu',
}
LIMIT = 500 * 1024 * 1024


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def version():
    return re.search(r'^version = "([^"]+)"', (ROOT / 'Cargo.toml').read_text(), re.M)[1]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fingerprint():
    paths = [ROOT / p for p in ('Cargo.toml', 'Cargo.lock', 'bindings/bkndb.h',
                               'scripts/go/package.py', '.github/workflows/go.yml')]
    paths += [p for p in (ROOT / 'crates').rglob('*')
              if p.is_file() and p.suffix in ('.rs', '.toml', '.udl', '.h', '.c')]
    paths += list(MODULE.glob('*.go')) + [MODULE / 'go.mod']
    h = hashlib.sha256()
    for p in sorted(set(paths), key=lambda item: item.relative_to(ROOT).as_posix()):
        h.update(p.relative_to(ROOT).as_posix().encode() + b'\0')
        h.update(p.read_bytes().replace(b'\r\n', b'\n') + b'\0')
    return h.hexdigest()


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + '\n')


def check_header():
    if (ROOT / 'bindings/bkndb.h').read_bytes() != (MODULE / 'bkndb.h').read_bytes():
        raise ValueError('Go header differs from bindings/bkndb.h; run package.py build')


def build(platform):
    target = TARGETS[platform]
    env = os.environ.copy()
    env['MACOSX_DEPLOYMENT_TARGET'] = '11.0'
    if env.get('RUSTFLAGS') or env.get('CARGO_ENCODED_RUSTFLAGS'):
        raise ValueError('Distribution builds require empty RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS')
    with tempfile.TemporaryFile(mode='w+') as log:
        result = subprocess.run(
            ['cargo', 'rustc', '--locked', '-p', 'bkndb-ffi', '--lib', '--profile',
             'go-dist', '--target', target, '--', '--print=native-static-libs'],
            cwd=ROOT, env=env, text=True, stdout=log, stderr=log)
        log.seek(0)
        native_output = log.read()
    print(native_output)
    result.check_returncode()
    native = re.findall(r'native-static-libs:\s*([^\n]+)', native_output)
    if not native:
        raise ValueError('rustc did not report native-static-libs')
    output = Path(env.get('CARGO_TARGET_DIR', ROOT / 'target'))
    if not output.is_absolute():
        output = ROOT / output
    archive = output / target / 'go-dist/libbkndb_ffi.a'
    dest = MODULE / 'lib' / platform
    dest.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(archive, dest / archive.name)
    shutil.copyfile(ROOT / 'bindings/bkndb.h', MODULE / 'bkndb.h')
    write_json(dest / 'build.json', {
        'version': version(), 'platform': platform, 'target': target,
        'sha256': digest(dest / archive.name), 'source_fingerprint': fingerprint(),
        'rustc': run(['rustc', '-Vv'], capture_output=True).stdout.strip(),
        'native_static_libs': native[-1].strip(),
        'baseline': run(['getconf', 'GNU_LIBC_VERSION'], capture_output=True).stdout.strip()
                    if platform.startswith('linux') else
                    'macOS 11.0' if platform.startswith('darwin') else 'Windows GNU / MinGW-w64',
    })
    # Linking the consumer in CI validates these flags against rustc's reported list.
    print(f'Built {platform}: {dest / archive.name}')


def verify(require_all=False):
    check_header()
    expected = fingerprint()
    records = {}
    for platform, target in TARGETS.items():
        directory = MODULE / 'lib' / platform
        metadata = directory / 'build.json'
        if not metadata.exists():
            if require_all:
                raise ValueError(f'Missing target {platform}')
            continue
        record = json.loads(metadata.read_text())
        for key, value in {'version': version(), 'platform': platform, 'target': target,
                           'source_fingerprint': expected,
                           'sha256': digest(directory / 'libbkndb_ffi.a')}.items():
            if record.get(key) != value:
                raise ValueError(f'{platform}: stale or invalid {key}; rebuild distribution')
        if not (directory / 'libbkndb_ffi.a').read_bytes().startswith(b'!<arch>\n'):
            raise ValueError(f'{platform}: not a regular static archive (Git LFS is unsupported)')
        if require_all and platform.startswith('linux') and record.get('baseline') != 'glibc 2.28':
            raise ValueError(f'{platform}: release archive must be built with glibc 2.28')
        if require_all and not record.get('rustc', '').startswith('rustc 1.99.1 '):
            raise ValueError(f'{platform}: release archive requires the pinned Rust 1.99.1 toolchain')
        records[platform] = record
    if not records:
        raise ValueError('No static archives; run package.py build <platform>')
    if require_all:
        manifest = json.loads((MODULE / 'lib/manifest.json').read_text())
        if manifest != {'version': version(), 'source_fingerprint': expected, 'targets': records}:
            raise ValueError('Distribution manifest is stale; run package.py assemble')
    return records


def assemble():
    records = verify()
    if set(records) != set(TARGETS):
        raise ValueError('All five targets are required to assemble a release')
    for platform, record in records.items():
        if platform.startswith('linux') and record['baseline'] != 'glibc 2.28':
            raise ValueError(f'{platform}: prepare releases in the manylinux 2.28 container')
    write_json(MODULE / 'lib/manifest.json', {
        'version': version(), 'source_fingerprint': fingerprint(), 'targets': records})
    verify(True)


def module_zip(proxy):
    """Use a strict file allowlist; no source-tree Rust files or symlinks enter the ZIP."""
    files = [p for p in MODULE.rglob('*') if p.is_file() and (
        (p.parent == MODULE and (p.suffix in ('.go', '.h', '.md') or p.name == 'go.mod'))
        or (p.parent == MODULE / 'example' and p.suffix == '.go')
        or (p.is_relative_to(MODULE / 'lib') and p.suffix in ('.a', '.json')))]
    # The repository license also belongs in the nested module's distribution.
    license_path = ROOT / 'LICENSE'
    entries = [(p, p.relative_to(MODULE).as_posix()) for p in files]
    if license_path.is_file():
        entries.append((license_path, 'LICENSE'))
    if any(p.is_symlink() for p, _ in entries):
        raise ValueError('Module must not contain symlinks')
    if sum(p.stat().st_size for p, _ in entries) > LIMIT:
        raise ValueError('Uncompressed Go module exceeds 500 MiB')
    v = 'v' + version()
    # Go proxy protocol escapes uppercase characters in module paths.
    escaped = ''.join('!' + c.lower() if c.isupper() else c for c in MODULE_PATH)
    dest = proxy / escaped / '@v'
    dest.mkdir(parents=True, exist_ok=True)
    zpath = dest / (v + '.zip')
    with zipfile.ZipFile(zpath, 'w', zipfile.ZIP_DEFLATED) as z:
        for p, name in sorted(entries, key=lambda item: item[1]):
            z.write(p, f'{MODULE_PATH}@{v}/{name}')
    if zpath.stat().st_size > LIMIT:
        raise ValueError('Compressed Go module exceeds 500 MiB')
    (dest / (v + '.mod')).write_bytes((MODULE / 'go.mod').read_bytes())
    write_json(dest / (v + '.info'), {'Version': v, 'Time': '2026-01-01T00:00:00Z'})
    (dest / 'list').write_text(v + '\n')
    return v


def check_binary(binary):
    if os.name == 'nt':
        tool = shutil.which('objdump')
        if not tool:
            raise ValueError('objdump is required to inspect Windows imports')
        result = run([tool, '-p', str(binary)], capture_output=True).stdout
    elif os.uname().sysname == 'Darwin':
        result = run(['otool', '-L', str(binary)], capture_output=True).stdout
    else:
        result = run(['readelf', '-d', str(binary)], capture_output=True).stdout
    if os.name == 'nt':
        # objdump also prints every unwind record; keep CI logs to DLL imports.
        print('Windows DLL imports:', ', '.join(re.findall(r'DLL Name:\s*(\S+)', result)))
    else:
        print(result)
    if re.search(r'(?:lib)?bkndb_ffi\.(?:dll|so|dylib)', result, re.I):
        raise ValueError('Executable still depends on a shared BknDb library')


def runtime_path(env, windows):
    if not windows:
        return '/nonexistent'
    # os.environ is case-insensitive on Windows, but its copied dict is not.
    root = env.get('SYSTEMROOT') or env.get('SystemRoot')
    if not root:
        raise ValueError('Windows SYSTEMROOT is required for system DLL lookup')
    return str(Path(root) / 'System32')


def consumer():
    verify()
    with tempfile.TemporaryDirectory(prefix='bkndb-consumer-') as directory:
        base = Path(directory).resolve()
        proxy = base / 'proxy'
        v = module_zip(proxy)
        work = base / 'consumer'
        work.mkdir()
        (work / 'go.mod').write_text(f'module consumer\n\ngo 1.22\n\nrequire {MODULE_PATH} {v}\n')
        (work / 'main.go').write_text('''package main
import (
    "fmt"
    "os"
    "path/filepath"
    b "github.com/BknOrg/bkndb/bindings/go/bkndb"
)
func must(err error) { if err != nil { panic(err) } }
func main() {
    dir, err := os.MkdirTemp("", "bkndb-smoke-"); must(err); defer os.RemoveAll(dir)
    path := filepath.Join(dir, "test.bkndb")
    db, err := b.Open(path); must(err)
    _, err = db.Exec("CREATE TABLE smoke (id INT PRIMARY KEY, name TEXT)"); must(err)
    tx, err := db.Begin(); must(err)
    _, err = tx.Exec("INSERT INTO smoke (id, name) VALUES (1, 'persisted')"); must(err)
    must(tx.Commit()); must(db.Close())
    db, err = b.Open(path); must(err)
    rows, err := db.SQL("SELECT name FROM smoke WHERE id = 1"); must(err)
    if len(rows.Rows) != 1 || rows.Rows[0][0] != "persisted" { panic("persistence failed") }
    must(db.Close()); fmt.Println("static consumer OK")
}
''')
        env = os.environ.copy()
        for name in ('LD_LIBRARY_PATH', 'DYLD_LIBRARY_PATH', 'CGO_LDFLAGS', 'CGO_CFLAGS',
                     'GOFLAGS', 'GOOS', 'GOARCH'):
            env.pop(name, None)
        env.update(GOPROXY=proxy.as_uri(), GOSUMDB='off', GOWORK='off', CGO_ENABLED='1',
                   GOMODCACHE=str(base / 'modcache'), GOCACHE=str(base / 'gocache'))
        run(['go', 'get', MODULE_PATH + '@' + v], cwd=work, env=env)
        run(['go', 'test', MODULE_PATH + '/...'], cwd=work, env=env)
        binary = work / ('smoke.exe' if os.name == 'nt' else 'smoke')
        run(['go', 'build', '-buildvcs=false', '-o', str(binary), '.'], cwd=work, env=env)
        check_binary(binary)
        clean = base / 'runtime'
        clean.mkdir()
        executable = clean / binary.name
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        # Keep Windows system DLL lookup, but exclude all build tools from PATH.
        env['PATH'] = runtime_path(env, os.name == 'nt')
        run([str(executable)], cwd=clean, env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['build', 'verify', 'assemble', 'consumer', 'zip', 'header'])
    parser.add_argument('platform', nargs='?', choices=TARGETS)
    parser.add_argument('--all', action='store_true', help='Require the complete release manifest')
    args = parser.parse_args()
    if args.command == 'build':
        if not args.platform:
            parser.error('build requires a platform')
        build(args.platform)
    elif args.command == 'verify':
        verify(args.all)
    elif args.command == 'assemble':
        assemble()
    elif args.command == 'consumer':
        consumer()
    elif args.command == 'header':
        check_header()
    elif args.command == 'zip':
        verify(args.all)
        with tempfile.TemporaryDirectory() as temp:
            module_zip(Path(temp))
    print(f'{args.command}: OK')


if __name__ == '__main__':
    main()
