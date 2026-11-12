//! Analyzes arbitrary text as a CUE sheet. The first 8 bytes give the size
//! every referenced file reports; the rest is the sheet. Analysis may fail,
//! but must never panic.

#![no_main]

use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;
use sprite_shrink_cd::analyze_cue_sheets;

fuzz_target!(|data: &[u8]| {
    let Some((size, sheet)) = data.split_first_chunk::<8>() else {
        return;
    };
    let size = u64::from_le_bytes(*size);
    let sheet = String::from_utf8_lossy(sheet).into_owned();

    let sheets = [(PathBuf::from("disc.cue"), sheet)];
    // Only a panic is a failure, so the result is dropped.
    drop(analyze_cue_sheets(&sheets, |_| Ok(size)));
});
