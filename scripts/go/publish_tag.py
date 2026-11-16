#!/usr/bin/env python3
"""Publish an immutable Go tag; retries succeed only for the same commit."""
import argparse
import re
import subprocess


def git(*args, cwd=None):
    return subprocess.run(['git', *args], cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.strip()


def remote_commit(remote, tag, cwd=None):
    output = git('ls-remote', '--tags', remote, f'refs/tags/{tag}',
                 f'refs/tags/{tag}^{{}}', cwd=cwd)
    refs = dict(line.split(None, 1)[::-1] for line in output.splitlines())
    return refs.get(f'refs/tags/{tag}^{{}}', refs.get(f'refs/tags/{tag}'))


def publish(version, expected_commit, remote='origin', cwd=None):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Expected a semantic version such as 0.3.1')
    commit = git('rev-parse', '--verify', f'{expected_commit}^{{commit}}', cwd=cwd)
    if git('rev-parse', 'HEAD', cwd=cwd) != commit:
        raise ValueError('Checkout differs from the validated release commit')
    tag = f'bindings/go/bkndb/v{version}'
    existing = remote_commit(remote, tag, cwd)
    if existing:
        if existing != commit:
            raise ValueError(f'{tag} already points to another commit; refusing to move it')
        print(f'{tag} already points to {commit}; release retry succeeded')
        return
    # No force and no local tag mutation. A competing publisher cannot replace a tag.
    pushed = subprocess.run(['git', 'push', remote, f'{commit}:refs/tags/{tag}'],
                            cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    # A network failure can happen after the server accepted the push. Reconcile
    # with remote state before declaring failure, including on workflow retries.
    actual = remote_commit(remote, tag, cwd)
    if actual != commit:
        if actual:
            raise ValueError(f'{tag} was published at another commit; refusing to move it')
        raise RuntimeError(f'Tag push failed; safe to retry: {pushed.stderr.strip()}')
    print(f'Published {tag} at {commit}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    args = parser.parse_args()
    publish(args.version, args.commit)


if __name__ == '__main__':
    main()
