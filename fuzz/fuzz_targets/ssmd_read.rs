//! Reads arbitrary bytes as a whole SSMD archive the way an extractor does:
//! parse the prefix, then for each disc, read sectors at random and write
//! the disc out. Every step may fail, but none may panic.

#![no_main]

use std::{
    cell::Cell,
    io::{self, BufWriter},
};

use libfuzzer_sys::fuzz_target;
use sprite_shrink::ChunkLocation;
use sprite_shrink_cd::{DiscReconstructor, parse_ssmd_prefix, write_disc};

/// Discs read per input.
const MAX_DISCS: usize = 4;

/// Sectors read per disc, through each read path, so one input can't run
/// for minutes.
const MAX_SECTORS: u32 = 64;

fuzz_target!(|data: &[u8]| {
    let Ok(parsed) = parse_ssmd_prefix::<u64>(data) else {
        return;
    };

    // The input is the whole archive, so stored data is read from it, as a
    // host reads it from the archive file.
    let read_stored = |location: ChunkLocation| {
        usize::try_from(location.offset).ok()
            .and_then(|start| {
                let end = start.checked_add(location.compressed_length as usize)?;
                data.get(start..end)
            })
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    };

    for manifest in parsed.disc_manifests.iter().take(MAX_DISCS) {
        if let Ok(reader) = DiscReconstructor::new(
            manifest,
            &parsed.indices,
            &parsed.data_dictionary,
            read_stored,
            &parsed.exception_blob,
            &parsed.subheader_table,
        ) {
            for sector in 0..MAX_SECTORS {
                // Only a panic is a failure, so the results are dropped.
                drop(reader.get_sector_data(sector));
            }
        }

        // Cancelling after `MAX_SECTORS` sectors bounds the run.
        let sectors_left = Cell::new(MAX_SECTORS);
        let mut sink = BufWriter::new(io::sink());
        drop(write_disc(
            manifest,
            &parsed.indices,
            &parsed.data_dictionary,
            read_stored,
            &parsed.exception_blob,
            &parsed.subheader_table,
            &mut sink,
            1,
            || {
                let left = sectors_left.get();
                sectors_left.set(left.saturating_sub(1));
                left == 0
            },
            |_| {},
        ));
    }
});
