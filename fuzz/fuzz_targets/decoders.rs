#![no_main]
libfuzzer_sys::fuzz_target!(|input:&[u8]| {
 if let Some((&kind,data))=input.split_first(){bkndb_engine::fuzzing::decode(kind,data);}
});
