"""Regression checks for rejecting broken or stale Go release packages."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package


class DistributionTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        module = root / 'bindings/go/bkndb'
        module.mkdir(parents=True)
        for name, content in {
            'Cargo.toml': 'version = "0.3.1"\n', 'Cargo.lock': 'lock',
            'bindings/bkndb.h': 'header', 'scripts/go/package.py': 'build script',
            '.github/workflows/go.yml': 'workflow',
            'bindings/go/bkndb/go.mod': f'module {package.MODULE_PATH}\n\ngo 1.25.0\n',
            'bindings/go/bkndb/binding_cgo.go': 'package bkndb\n',
            'bindings/go/bkndb/bkndb.h': 'header',
            'LICENSE': 'license',
        }.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        (root / 'crates').mkdir()
        self.patchers = [patch.object(package, 'ROOT', root), patch.object(package, 'MODULE', module)]
        for patcher in self.patchers:
            patcher.start()
            self.addCleanup(patcher.stop)
        self.module = module
        for platform, target in package.TARGETS.items():
            directory = module / 'lib' / platform
            directory.mkdir(parents=True)
            archive = directory / 'libbkndb_ffi.a'
            archive.write_bytes(b'!<arch>\nfixture')
            package.write_json(directory / 'build.json', {
                'platform': platform, 'target': target, 'version': '0.3.1',
                'sha256': package.digest(archive), 'source_fingerprint': package.fingerprint(),
                'baseline': 'glibc 2.28' if platform.startswith('linux') else 'native',
                'rustc': 'rustc 1.99.1 (fixture)',
            })
        dest=module/'wasm';dest.mkdir();(dest/'bkndb.wasm').write_bytes(b'\0asm\x01\0\0\0')
        package.write_json(dest/'manifest.json',{'abi':1,'version':'0.3.1','target':'wasm32-unknown-unknown','rustc':'rustc 1.99.1 (fixture)','sha256':package.digest(dest/'bkndb.wasm'),'source_fingerprint':package.fingerprint()})
        package.assemble()

    def record(self, platform, **updates):
        path = self.module / 'lib' / platform / 'build.json'
        data = json.loads(path.read_text())
        data.update(updates)
        package.write_json(path, data)

    def test_windows_runtime_path_from_copied_environment(self):
        for spelling in ('SYSTEMROOT', 'SystemRoot'):
            path = package.runtime_path({spelling: 'C:/Windows'}, True)
            self.assertEqual(Path(path), Path('C:/Windows') / 'System32')
        self.assertEqual(package.runtime_path({}, False), '/nonexistent')
        with self.assertRaisesRegex(ValueError, 'SYSTEMROOT'):
            package.runtime_path({}, True)

    def test_complete_distribution(self):
        self.assertEqual(set(package.verify(True)), set(package.TARGETS))

    def test_corrupt_archive(self):
        (self.module / 'lib/linux_amd64/libbkndb_ffi.a').write_bytes(b'corrupt')
        with self.assertRaisesRegex(ValueError, 'sha256'):
            package.verify(True)

    def test_source_change_requires_rebuild(self):
        (self.module / 'binding_cgo.go').write_text('package bkndb // changed\n')
        with self.assertRaisesRegex(ValueError, 'source_fingerprint'):
            package.verify(True)

    def test_header_drift(self):
        (self.module / 'bkndb.h').write_text('stale')
        with self.assertRaisesRegex(ValueError, 'header differs'):
            package.verify(True)

    def test_missing_target(self):
        (self.module / 'lib/windows_amd64/build.json').unlink()
        with self.assertRaisesRegex(ValueError, 'Missing target windows_amd64'):
            package.verify(True)

    def test_wrong_release_baseline(self):
        self.record('linux_amd64', baseline='glibc 2.39')
        with self.assertRaisesRegex(ValueError, 'glibc 2.28'):
            package.verify(True)

    def test_wrong_toolchain(self):
        self.record('windows_amd64', rustc='rustc 1.98.0 (fixture)')
        with self.assertRaisesRegex(ValueError, 'pinned Rust'):
            package.verify(True)

    def test_lfs_pointer_even_with_matching_checksum(self):
        archive = self.module / 'lib/linux_amd64/libbkndb_ffi.a'
        archive.write_text('version https://git-lfs.github.com/spec/v1\n')
        self.record('linux_amd64', sha256=package.digest(archive))
        with self.assertRaisesRegex(ValueError, 'not a regular static archive'):
            package.verify(True)

    def test_zip_proxy_layout_and_contents(self):
        proxy = Path(self.temp.name) / 'proxy'
        package.module_zip(proxy)
        zpath = proxy / 'github.com/!bkn!org/bkndb/bindings/go/bkndb/@v/v0.3.1.zip'
        with zipfile.ZipFile(zpath) as z:
            prefix = package.MODULE_PATH + '@v0.3.1/'
            self.assertIn(prefix + 'bkndb.h', z.namelist())
            self.assertIn(prefix + 'LICENSE', z.namelist())
            self.assertIn(prefix + 'wasm/bkndb.wasm', z.namelist())
            for platform in package.TARGETS:
                self.assertIn(prefix + f'lib/{platform}/libbkndb_ffi.a', z.namelist())
            self.assertFalse(any('crates/' in name for name in z.namelist()))

    def test_stale_wasm_is_rejected(self):
        (self.module/'wasm/bkndb.wasm').write_bytes(b'corrupt')
        with self.assertRaisesRegex(ValueError,'WASM sha256'):
            package.verify(True)

    def test_zip_size_limit(self):
        with patch.object(package, 'LIMIT', 8):
            with self.assertRaisesRegex(ValueError, 'exceeds 500 MiB'):
                package.module_zip(Path(self.temp.name) / 'proxy')

    def test_symlink_is_rejected(self):
        link = self.module / 'linked.h'
        try:
            link.symlink_to(self.module / 'bkndb.h')
        except OSError:
            self.skipTest('Symlink creation is unavailable')
        with self.assertRaisesRegex(ValueError, 'symlinks'):
            package.module_zip(Path(self.temp.name) / 'proxy')


if __name__ == '__main__':
    unittest.main()
