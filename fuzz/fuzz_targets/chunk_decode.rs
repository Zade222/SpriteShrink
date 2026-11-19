//! Decodes arbitrary bytes as one zstd chunk, with a length limit taken
//! from the input. Decoding may fail, but it must never return more than
//! the limit, and must not panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sprite_shrink::{decompress_chunk, decompress_chunk_checked};

fuzz_target!(|data: &[u8]| {
    // A limit of at most 64 KiB keeps each run fast; the clamp to
    // `MAX_CHUNK_LEN` for larger limits has its own unit test.
    let Some((limit, frame)) = data.split_first_chunk::<2>() else {
        return;
    };
    let max_len = usize::from(u16::from_le_bytes(*limit));

    if let Ok(decoded) = decompress_chunk(frame, &[], max_len) {
        assert!(
            decoded.len() <= max_len,
            "decoded {} bytes with a limit of {max_len}",
            decoded.len()
        );
    }

    // Only a panic is a failure; the hash almost never matches.
    drop(decompress_chunk_checked(frame, &[], 0_u64, max_len));
});
