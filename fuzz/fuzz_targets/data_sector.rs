//! Analyzes an arbitrary 2352-byte sector as each data sector type the
//! mapper passes in. Analysis may fail, but must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sprite_shrink_cd::{SectorType, analyze_data_sector};

/// The types the mapper analyzes a data sector as.
const DATA_TYPES: [SectorType; 4] = [
    SectorType::Mode1,
    SectorType::Mode2Form1,
    SectorType::PregapMode1,
    SectorType::PregapMode2,
];

fuzz_target!(|sector: [u8; 2352]| {
    for sector_type in DATA_TYPES {
        // Only a panic is a failure, so the result is dropped.
        drop(analyze_data_sector(&sector, sector_type));
    }
});
