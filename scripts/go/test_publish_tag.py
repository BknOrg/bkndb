"""Exercise retry and immutability behavior using real temporary Git remotes."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import publish_tag


class PublishTagTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.remote = base / 'remote.git'
        self.work = base / 'work'
        self.work.mkdir()
        # Keep the temporary repositories independent of the caller's checkout.
        env = os.environ.copy()
        for key in list(env):
            if key.startswith('GIT_'):
                env.pop(key)
        self.environment = patch.dict(os.environ, env, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.command('init', '--bare', str(self.remote))
        self.command('init', str(self.work))
        self.command('config', 'user.name', 'Release test', cwd=self.work)
        self.command('config', 'user.email', 'release-test@example.invalid', cwd=self.work)
        self.command('remote', 'add', 'origin', str(self.remote), cwd=self.work)
        self.commit = self.new_commit('first')
        self.tag = 'bindings/go/bkndb/v0.3.1'

    def command(self, *args, cwd=None):
        return subprocess.run(['git', *args], cwd=cwd, check=True, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.strip()

    def new_commit(self, content):
        (self.work / 'source').write_text(content)
        self.command('add', 'source', cwd=self.work)
        self.command('commit', '-m', content, cwd=self.work)
        return self.command('rev-parse', 'HEAD', cwd=self.work)

    def test_publish_and_retry_same_commit(self):
        publish_tag.publish('0.3.1', self.commit, cwd=self.work)
        publish_tag.publish('0.3.1', self.commit, cwd=self.work)
        self.assertEqual(publish_tag.remote_commit('origin', self.tag, self.work), self.commit)

    def test_refuse_to_replace_published_tag(self):
        publish_tag.publish('0.3.1', self.commit, cwd=self.work)
        new = self.new_commit('second')
        with self.assertRaisesRegex(ValueError, 'another commit'):
            publish_tag.publish('0.3.1', new, cwd=self.work)
        self.assertEqual(publish_tag.remote_commit('origin', self.tag, self.work), self.commit)

    def test_refuse_unvalidated_checkout(self):
        self.new_commit('second')
        with self.assertRaisesRegex(ValueError, 'validated release commit'):
            publish_tag.publish('0.3.1', self.commit, cwd=self.work)
        self.assertIsNone(publish_tag.remote_commit('origin', self.tag, self.work))

    def test_retry_annotated_tag(self):
        self.command('tag', '-a', self.tag, '-m', 'release', cwd=self.work)
        self.command('push', 'origin', self.tag, cwd=self.work)
        publish_tag.publish('0.3.1', self.commit, cwd=self.work)

    def test_network_failure_after_accepted_push(self):
        original = subprocess.run
        def lose_response(args, **kwargs):
            result = original(args, **kwargs)
            if args[:2] == ['git', 'push']:
                return subprocess.CompletedProcess(args, 1, '', 'response lost')
            return result
        with patch.object(publish_tag.subprocess, 'run', side_effect=lose_response):
            publish_tag.publish('0.3.1', self.commit, cwd=self.work)
        self.assertEqual(publish_tag.remote_commit('origin', self.tag, self.work), self.commit)


if __name__ == '__main__':
    unittest.main()
