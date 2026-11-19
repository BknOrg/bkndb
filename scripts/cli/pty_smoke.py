#!/usr/bin/env python3
"""Exercise the actual Unix terminal editor, completion, Ctrl-C and rollback."""
import argparse
import os
from pathlib import Path
import select
import signal
import tempfile
import time


def run(binary):
    if os.name != 'posix':
        raise RuntimeError('PTY smoke requires Unix; Windows still runs piped shell tests')
    import pty
    import subprocess
    binary = str(Path(binary).resolve())
    with tempfile.TemporaryDirectory(prefix='bkndb-cli-pty-') as tmp:
        database = str(Path(tmp) / 'data')
        subprocess.run([binary, 'create', database, '--backend', 'lsm'], check=True, capture_output=True)
        subprocess.run([binary, 'sql', database, '--write', '--query', 'CREATE TABLE t (id INT PRIMARY KEY)'], check=True, capture_output=True)
        pid, fd = pty.fork()
        if pid == 0:
            os.execv(binary, [binary, 'shell', database, '--write'])
        transcript = bytearray()

        def receive(expected):
            received = bytearray()
            deadline = time.monotonic() + 15
            while expected not in received:
                if time.monotonic() >= deadline:
                    raise AssertionError(f'terminal timeout for {expected!r}: {bytes(transcript)!r}')
                ready, _, _ = select.select([fd], [], [], 0.2)
                if ready:
                    chunk = os.read(fd, 65536)
                    if not chunk:
                        raise AssertionError('terminal closed early')
                    received.extend(chunk)
                    transcript.extend(chunk)

        try:
            receive(b'bkndb> ')
            # Redrawing each key also prints the prompt after ANSI cursor
            # controls. Match a NEW LINE prompt only, otherwise the next
            # command can arrive before Enter was handled/raw mode restored.
            for text, prompt in [(b'.inf\t\r', b'\r\nbkndb> '), (b'BEGIN;\r', b'\r\ntx> '),
                                 (b'INSERT INTO t VALUES (1);\r', b'\r\ntx> '),
                                 (b'\x03', b'\r\nbkndb> '),
                                 (b'SELE\t * FROM t;\r', b'\r\nbkndb> ')]:
                os.write(fd, text)
                receive(prompt)
            os.write(fd, b'.quit\r')
            deadline = time.monotonic() + 15
            while True:
                completed, status = os.waitpid(pid, os.WNOHANG)
                if completed:
                    assert os.waitstatus_to_exitcode(status) == 0
                    pid = None
                    break
                if time.monotonic() >= deadline:
                    raise AssertionError('shell did not exit')
                time.sleep(0.05)
            import json
            result = subprocess.run([binary, 'sql', database, '--query', 'SELECT COUNT(*) FROM t', '--format', 'json'], check=True, capture_output=True, text=True)
            assert json.loads(result.stdout)['data']['rows'][0][0]['Int'] == 0
        finally:
            if pid:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(fd)
    print('PTY smoke: OK')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    run(parser.parse_args().binary)
