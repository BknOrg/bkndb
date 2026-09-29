//! Commit path: WAL append, memtable flush and compaction.
use super::*;

impl LsmStorageBackend {
    pub(super) fn write_options(&self) -> WriteOptions {
        WriteOptions {
            block_size: self.options.block_size_bytes,
            compress: self.options.compression,
        }
    }

    pub(super) fn snapshot(&self) -> ReadSnapshot {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        ReadSnapshot {
            active_memtable: state.active_memtable.clone(),
            immutable_memtables: state.immutable_memtables.clone(),
            sstables: state.sstables.clone(),
        }
    }

    pub(super) fn commit_pending(&self, pending: Pending) -> Result<(), BknError> {
        if pending.is_empty() {
            return Ok(());
        }

        let record = WalRecord {
            entries: pending.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        {
            let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
            wal::append(&cw.file, &record)?;
        }

        let needs_flush;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            {
                // `Arc::make_mut`: cheap in-place mutation unless a reader
                // snapshot is currently outstanding, in which case it
                // clones first — clone-on-write, not clone-always.
                let map = Arc::make_mut(&mut state.active_memtable);
                for (key, value) in pending {
                    match value {
                        Some(bytes) => {
                            map.insert(key, LsmValue::Value(bytes));
                        }
                        None => {
                            map.insert(key, LsmValue::Tombstone);
                        }
                    }
                }
            }
            needs_flush = memtable_byte_size(&state.active_memtable) >= self.options.memtable_flush_bytes;
        }

        if needs_flush {
            self.flush()?;
        }
        Ok(())
    }

    /// Appends the flushed memtable as a new SSTable blob and a new
    /// manifest blob at the container file's current end, one `sync_data`
    /// covering both, then a header write pointing at the new manifest —
    /// that header write is the actual commit point for the flush. Old WAL
    /// bytes before the new `wal_region_start` are left as dead space,
    /// reclaimed only by the next compaction (see `compact_all`), per the
    /// confirmed "compaction is also the vacuum point" design.
    pub(super) fn flush(&self) -> Result<(), BknError> {
        let memtable_to_flush;
        let generation;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.active_memtable.is_empty() {
                return Ok(());
            }
            memtable_to_flush = state.active_memtable.clone();
            state.immutable_memtables.push(memtable_to_flush.clone());
            state.active_memtable = Arc::new(Memtable::new());
            generation = state.next_sstable_id;
            state.next_sstable_id += 1;
        }

        let new_handle;
        let new_wal_region_start;
        let new_manifest_len;
        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());

            let entries = memtable_to_flush.iter().map(|(k, v)| Ok((k.clone(), v.clone())));
            let handle = SstableHandle::write(&cw.file, generation, entries, memtable_to_flush.len(), self.write_options())?;

            let (mut sstable_refs, next_sstable_id) = {
                let state = self.state.read().unwrap_or_else(|e| e.into_inner());
                let refs: Vec<SstableRef> = state
                    .sstables
                    .iter()
                    .map(|s| SstableRef {
                        generation: s.generation,
                        offset: s.base_offset,
                        length: s.blob_len,
                    })
                    .collect();
                (refs, state.next_sstable_id)
            };
            sstable_refs.push(SstableRef {
                generation: handle.generation,
                offset: handle.base_offset,
                length: handle.blob_len,
            });

            let manifest_offset = handle.base_offset + handle.blob_len;
            let placeholder = Manifest {
                next_sstable_id,
                sstables: sstable_refs,
                wal_region_start: 0,
            };
            let len = placeholder.encode()?.len() as u64;
            let manifest = Manifest {
                wal_region_start: manifest_offset + len,
                ..placeholder
            };
            let bytes = manifest.encode()?;
            container::write_blob_at(&cw.file, manifest_offset, &bytes)?;
            cw.file.sync_data().map_err(io_err)?;
            let (seq, slot) = container::write_header(&cw.file, cw.header_seq, cw.header_slot, manifest_offset, &bytes)?;
            cw.header_seq = seq;
            cw.header_slot = slot;

            new_handle = handle;
            new_wal_region_start = manifest.wal_region_start;
            new_manifest_len = bytes.len() as u64;
        }

        let should_compact;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables.push(Arc::new(new_handle));
            if let Some(pos) = state.immutable_memtables.iter().position(|m| Arc::ptr_eq(m, &memtable_to_flush)) {
                state.immutable_memtables.remove(pos);
            }
            state.wal_region_start = new_wal_region_start;
            state.manifest_len = new_manifest_len;
            should_compact = state.sstables.len() >= self.options.compaction_trigger_files;
        }

        if should_compact {
            self.compact_all(false)?;
        }
        Ok(())
    }

    /// Merges every current SSTable generation into one new SSTable blob,
    /// writing it into a **brand-new file** (header + merged blob + a
    /// manifest referencing just that one SSTable + an empty WAL region)
    /// rather than appending to the live file, then atomically renaming
    /// that new file over the live path. This is also the vacuum step: the
    /// new file only contains live data, so it's smaller than the file it
    /// replaces whenever there was dead space to reclaim — and it's written
    /// in the current SSTable format, which upgrades legacy files.
    ///
    /// The merge streams: each input SSTable is read one block at a time
    /// and the output is written block by block, so memory stays flat no
    /// matter how large the database is.
    ///
    /// Because this always merges *all* generations at once (size-tiered,
    /// "compact everything past N files" rather than a partial/leveled
    /// merge), no older generation is ever left outside the batch that
    /// could still need a tombstone to shadow it — so dropping tombstones
    /// in the merge is always safe here.
    ///
    /// Must be called only while `writer_lock` is already held by the
    /// caller (either inherited from a write-tx guard via `flush()`, or
    /// acquired explicitly by `force_compact()`) — it does not acquire the
    /// lock itself, to avoid deadlocking when `flush()` calls it.
    ///
    /// A `ReadSnapshot` taken before this call holds `Arc<SstableHandle>`s
    /// that each independently own their own `Arc<File>`, captured at
    /// construction time and fully decoupled from `self.path`. Reading
    /// from them after the rename below is safe and correct: on POSIX, an
    /// open fd to an unlinked/renamed-away inode keeps working until
    /// closed; on Windows, `FILE_SHARE_DELETE` (set when every container
    /// file handle is opened, see `container.rs`) lets the rename proceed
    /// while such handles remain open, and NTFS defers freeing the old
    /// file's storage until the last handle to it closes.
    ///
    /// The automatic call from `flush` (`force == false`) only merges once
    /// there are at least two SSTables. `force_compact` rewrites even a lone
    /// SSTable: the point there is also to reclaim dead WAL/manifest space
    /// and upgrade legacy-format data, which one SSTable can still carry.
    pub(super) fn compact_all(&self, force: bool) -> Result<(), BknError> {
        let sstables_to_merge;
        {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            let worth_it = state.sstables.len() >= 2 || (force && !state.sstables.is_empty());
            if !worth_it {
                return Ok(());
            }
            sstables_to_merge = state.sstables.clone();
        }

        // Newest generation first, as `MergeIter` expects.
        let sources: Vec<EntryIter<'_>> = sstables_to_merge
            .iter()
            .rev()
            .map(|sst| Box::new(sst.cursor(Bound::Unbounded, Bound::Unbounded)) as EntryIter<'_>)
            .collect();
        let expected: u64 = sstables_to_merge.iter().map(|s| s.estimated_entries()).sum();

        let new_generation = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            let id = state.next_sstable_id;
            state.next_sstable_id += 1;
            id
        };

        let tmp_path = compaction_tmp_path(&self.path);
        let fresh = write_fresh_container(&tmp_path, new_generation, MergeIter::new(sources), expected as usize, self.write_options())?;

        lock_exclusive(&fresh.file, &self.path)?;
        fs::rename(&tmp_path, &self.path).map_err(io_err)?;
        sync_parent_dir(&self.path)?;

        {
            let mut cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
            cw.file = fresh.file.clone();
            cw.header_seq = 1;
            cw.header_slot = 0;
        }
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.sstables = vec![Arc::new(fresh.handle)];
            state.wal_region_start = fresh.manifest.wal_region_start;
            state.next_sstable_id = fresh.manifest.next_sstable_id;
            state.manifest_len = fresh.manifest_len;
        }
        Ok(())
    }
}
