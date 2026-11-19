#!/usr/bin/env python3
"""Build, verify and test the explicit CGO-free Go/WASM distribution."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import package


def build():
    if os.environ.get('RUSTFLAGS') or os.environ.get('CARGO_ENCODED_RUSTFLAGS'):
        raise ValueError('WASM distribution builds require empty Rust flags')
    package.run(['cargo','build','--locked','-p','bkndb-wasm','--target','wasm32-unknown-unknown','--profile','wasm-dist'],cwd=package.ROOT)
    output=Path(os.environ.get('CARGO_TARGET_DIR',package.ROOT/'target'))
    if not output.is_absolute(): output=package.ROOT/output
    dest=package.MODULE/'wasm';dest.mkdir(exist_ok=True)
    shutil.copyfile(output/'wasm32-unknown-unknown/wasm-dist/bkndb_wasm.wasm',dest/'bkndb.wasm')
    package.write_json(dest/'manifest.json',{'abi':1,'version':package.version(),'target':'wasm32-unknown-unknown','sha256':package.digest(dest/'bkndb.wasm'),'source_fingerprint':package.fingerprint(),'rustc':package.run(['rustc','-Vv'],capture_output=True).stdout.strip()})
    verify()


def verify(release=False):
    dest=package.MODULE/'wasm';record=json.loads((dest/'manifest.json').read_text())
    expected={'abi':1,'version':package.version(),'target':'wasm32-unknown-unknown','sha256':package.digest(dest/'bkndb.wasm'),'source_fingerprint':package.fingerprint()}
    for key,value in expected.items():
        if record.get(key)!=value:raise ValueError(f'stale WASM {key}; rebuild with scripts/go/wasm.py build')
    if release and not record.get('rustc','').startswith('rustc 1.99.1 '):raise ValueError('WASM release requires the pinned Rust 1.99.1 toolchain')
    if not (dest/'bkndb.wasm').read_bytes().startswith(b'\0asm\x01\0\0\0'):raise ValueError('not a WebAssembly v1 module')


def interchange():
    verify()
    with tempfile.TemporaryDirectory(prefix='bkndb-interchange-') as temp:
        path=str(Path(temp)/'exchange.bkndb');env=os.environ.copy();env['BKNDB_INTERCHANGE']=path;env['CGO_ENABLED']='0'
        cargo=['cargo','run','--locked','--release','-p','bkndb-storage-lsm','--example','interchange','--']
        package.run(cargo+['seed',path],cwd=package.ROOT)
        held=subprocess.Popen(cargo+['hold',path],cwd=package.ROOT,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        try:
            ready=False
            for line in held.stdout:
                if line.strip()=='READY':ready=True;break
            if not ready:raise RuntimeError('native lock fixture did not start')
            locked_env=env.copy();locked_env['BKNDB_INTERCHANGE_LOCKED']=path
            package.run(['go','test','-tags=bkndb_wasm','-run','^TestNativeWASMLocking$','-count=1','.'],cwd=package.MODULE,env=locked_env)
        finally:
            if held.poll() is None:
                held.stdin.write('\n');held.stdin.flush()
                try:held.wait(timeout=30)
                except subprocess.TimeoutExpired:held.kill();held.wait();raise
            if held.returncode!=0:raise RuntimeError('native lock fixture failed')
        package.run(['go','test','-tags=bkndb_wasm','-run','^TestNativeWASMInterchange$','-count=1','.'],cwd=package.MODULE,env=env)
        package.run(cargo+['verify',path],cwd=package.ROOT)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('command',choices=['build','verify','consumer','interchange']);a=p.parse_args()
    if a.command=='build':build()
    elif a.command=='verify':verify()
    elif a.command=='consumer':verify();package.consumer(wasm=True)
    else:interchange()
    print(a.command+': OK')
if __name__=='__main__':main()
