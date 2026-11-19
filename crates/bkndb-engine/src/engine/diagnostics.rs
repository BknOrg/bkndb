//! Read-only structural diagnostics. Payloads are deliberately omitted.
use super::*;

#[derive(Debug, serde::Serialize)]
pub struct Diagnostics {
    pub format_version:u32,
    pub selected_header_slot:u8,
    pub header_sequence:u64,
    pub manifest_offset:u64,
    pub manifest_bytes:u64,
    pub wal_start:u64,
    pub valid_wal_end:u64,
    pub file_bytes:u64,
    pub wal_records:u64,
    pub wal_entries:u64,
    pub sstables:Vec<SstableDiagnostic>,
}
#[derive(Debug, serde::Serialize)]
pub struct SstableDiagnostic {
    pub generation:u64,pub offset:u64,pub bytes:u64,pub estimated_entries:u64,pub legacy:bool,
}
impl LsmStorageBackend {
    /// Structural summary from validated decoders, with an explicit output cap.
    /// Does not expose key/value payloads or perform repair.
    pub fn diagnostics(&self)->Result<Diagnostics,BknError> {
        self.ensure_healthy()?;
        let cw=self.container.lock().map_err(|_|BknError::RecoveryRequired("poisoned container".into()))?;
        let header=container::read_header(&cw.file)?.ok_or_else(||BknError::Corruption("missing root".into()))?;
        let manifest=Manifest::decode(&container::read_manifest(&cw.file,&header)?)?;
        if manifest.sstables.len()>10000{return Err(BknError::ResourceLimit("diagnostics exceeds 10000 SSTables".into()));}
        let state=self.state.read().map_err(|_|BknError::RecoveryRequired("poisoned state".into()))?;
        let mut entries=0;
        let (records,end)=wal::replay_visit(&cw.file,manifest.wal_region_start,false,self.format_version,|r|{entries+=r.entries.len() as u64;Ok(())})?;
        Ok(Diagnostics {format_version:self.format_version,selected_header_slot:header.slot,header_sequence:header.seq,manifest_offset:header.manifest_offset,manifest_bytes:header.manifest_len,
            wal_start:manifest.wal_region_start,valid_wal_end:end,file_bytes:cw.file.metadata().map_err(io_err)?.len(),wal_records:records,wal_entries:entries,
            sstables:state.sstables.iter().map(|s|SstableDiagnostic{generation:s.generation,offset:s.base_offset,bytes:s.blob_len,estimated_entries:s.estimated_entries(),legacy:s.is_legacy()}).collect()})
    }
}
