#!/usr/bin/env python3
"""Small valid format fixtures so fuzzing reaches checksummed decoder paths."""
from pathlib import Path
import struct as s
import zlib
p=Path(__file__).parent/'corpus/decoders';p.mkdir(parents=True,exist_ok=True)
def le64(n):return s.pack('<Q',n)
def be32(n):return s.pack('>I',n)
def be64(n):return s.pack('>Q',n)
def vec(b):return le64(len(b))+b
def crc(b):return zlib.crc32(b)&0xffffffff
def save(kind,name,b): (p/name).write_bytes(bytes([kind])+b)
manifest=le64(1)+le64(0)+le64(152)
save(1,'valid-manifest',manifest)
slot=b'BKNDBLS1'+be32(3)+be64(1)+be64(128)+be64(len(manifest))+be32(crc(manifest));slot+=be32(crc(slot));slot+=bytes(64-len(slot))
save(0,'valid-header',slot+bytes(64)+manifest)
key=b'\x01tk';raw=be32(len(key))+key+b'\x01'+be32(1)+b'v'
payload=le64(1)+vec(key)+b'\x01'+vec(b'v')
header=b'BKW3'+be32(len(payload))+be32(crc(payload));header+=be32(crc(header))
save(2,'valid-wal',header+payload)
bloom=s.pack('<II',64,1)+vec(bytes([255])*8)
save(4,'valid-bloom',bloom);save(5,'valid-entry',raw)
for compressed in (False,True):
 stored=bytes([len(raw)<<4])+raw if compressed else raw
 block=vec(key)+le64(0)+s.pack('<II',len(stored),len(raw))+bytes([compressed])+s.pack('<I',crc(stored))
 index=vec(key)+vec(key)+le64(1)+le64(1)+block
 footer=be64(len(stored))+be64(len(stored)+len(bloom))+be64(len(index))+be32(crc(bloom+index))+be32(0)+b'BKNSST02'
 save(3,'valid-sstable-'+str(compressed).lower(),stored+bloom+index+footer)
legacy=vec(key)+vec(key)+le64(1)+vec(key)+le64(0)
save(3,'valid-sstable-legacy',raw+bloom+legacy+be64(len(raw))+be64(len(raw)+len(bloom))+be64(len(legacy)))
