//! Reads arbitrary bytes as a whole SSMC archive the way an extractor does:
//! parse the prefix, then for each file, plan seeks and decompress its
//! chunks. Every step may fail, but none may panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sprite_shrink::{decompress_chunk_checked, parse_ssmc_prefix};

/// Chunks decompressed per input, so one input can't run for minutes.
const MAX_CHUNKS: usize = 256;

fuzz_target!(|data: &[u8]| {
    let Ok(parsed) = parse_ssmc_prefix::<u64>(data) else {
        return;
    };

    let mut chunks_left = MAX_CHUNKS;
    for checked in &parsed.manifests {
        let size = checked.size();
        // Only a panic is a failure, so the results are dropped.
        drop(checked.seek_chunks(0, size));
        drop(checked.seek_chunks(size / 3, size / 3));

        for chunk in &checked.manifest().chunk_metadata {
            if chunks_left == 0 {
                return;
            }
            chunks_left -= 1;

            let Some(location) = parsed.chunk_index.get(&chunk.hash) else {
                continue;
            };
            let Ok(start) = usize::try_from(location.offset) else {
                continue;
            };
            let end = start.saturating_add(location.compressed_length as usize);
            let Some(compressed) = data.get(start..end) else {
                continue;
            };
            drop(decompress_chunk_checked(
                compressed,
                &parsed.dictionary,
                chunk.hash,
                chunk.length as usize,
            ));
        }
    }
});
