import json
import math
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import candidate


class CandidateTests(unittest.TestCase):
    def test_missing_evidence_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                candidate.validate(directory)

    def test_soak_duration_cannot_be_shortened(self):
        with self.assertRaises(ValueError):
            candidate.soak('unused', 'unused', 0.001)

    def test_nonfinite_soak_durations_are_rejected_before_running(self):
        for hours in (math.nan, math.inf, -math.inf):
            with self.subTest(hours=hours), self.assertRaisesRegex(ValueError, 'finite'):
                candidate.soak('unused', 'unused', hours)

    def test_stale_source_and_missing_gates_block_readiness(self):
        identity = {'schema_version': 1, 'version': 'test', 'source_fingerprint': 'abc'}
        with tempfile.TemporaryDirectory() as directory, patch.object(candidate, 'identity', return_value=identity):
            root = Path(directory)
            target = root / candidate.TARGETS[0]
            target.mkdir()
            (target / 'manifest.json').write_text(json.dumps(identity | {'source_fingerprint': 'stale'}))
            with self.assertRaisesRegex(ValueError, 'stale'):
                candidate.validate(root)

    def test_checksums_and_audit_are_required(self):
        identity = {'schema_version': 1, 'version': 'test', 'source_fingerprint': 'abc'}
        with tempfile.TemporaryDirectory() as directory, patch.object(candidate, 'identity', return_value=identity):
            root = Path(directory)
            for name in candidate.TARGETS:
                target = root / name
                target.mkdir()
                binary = b'test binary'
                filename = 'bkndb.exe' if 'windows' in name else 'bkndb'
                (target / filename).write_bytes(binary)
                report = identity | {'target': name, 'binary': filename, 'sha256': candidate.hashlib.sha256(binary).hexdigest(),
                    'rustc': 'rustc 1.99.1 test', 'native_tests': True, 'runtime_smoke': True, 'million_operations': True}
                (target / 'manifest.json').write_text(json.dumps(report))
            soak = identity | {'passed': True, 'continuous_seconds': 28800, 'iterations': 1,
                              'binary_sha256': candidate.hashlib.sha256(binary).hexdigest()}
            (root / 'soak.json').write_text(json.dumps(soak))
            for gate in ('engine', 'fuzz', 'audit'):
                (root / (gate + '.json')).write_text(json.dumps(identity | {'passed': True}))
            with self.assertRaisesRegex(ValueError, 'reviewed audit'):
                candidate.validate(root)
            (root / 'audit.json').write_text(json.dumps(identity | {'passed': True, 'critical_findings': [], 'reviewer': 'maintainer', 'scope': 'reviewed gates'}))
            candidate.validate(root)
            for invalid in (float('nan'), float('inf'), True, '28800', 28799):
                (root / 'soak.json').write_text(json.dumps(soak | {'continuous_seconds': invalid}))
                with self.subTest(duration=invalid), self.assertRaises(ValueError):
                    candidate.validate(root)
            (root / 'soak.json').write_text(json.dumps(soak | {'binary_sha256': 'unrelated'}))
            with self.assertRaisesRegex(ValueError, 'soak executable'):
                candidate.validate(root)
            (root / 'soak.json').write_text(json.dumps(soak))
            (root / candidate.TARGETS[0] / 'bkndb').write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError, 'checksum'):
                candidate.validate(root)


if __name__ == '__main__':
    unittest.main()
