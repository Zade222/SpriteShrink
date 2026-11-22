//! Builds and reads Sprite Shrink's SSMD archives of CD and GD-ROM disc
//! images: it parses CUE sheets, sorts each sector into audio, data or an
//! exception, stores audio as FLAC and data as deduplicated chunks, and
//! rebuilds the original sectors on extraction.
//!
//! The library does no file I/O. The application opens, reads and writes
//! files, and passes the library bytes, `Read + Seek` readers it opened
//! (`MultiBinStream` joins those of a multi-file disc), size callbacks and
//! read callbacks for stored data. Operating systems differ in how files
//! are opened, locked and read, and a host such as an emulator may not read
//! from files at all.

mod analyze;
pub use analyze::{
    FileSizeProvider, analyze_cue_sheets, resolve_file_sector_counts
};
/// Only for fuzz targets, which `cargo fuzz` builds with `--cfg fuzzing`.
#[cfg(fuzzing)]
pub use analyze::analyze_data_sector;

mod cue_gen;
pub use cue_gen::{
    generate_cue_string
};

mod cue_parser;

mod decode;
pub use decode::{
    decode_ssmd_format_data, parse_ssmd_prefix, parse_ssmd_toc
};

mod ecc;

mod encode;
pub use encode::{SSMDPrefix, build_ssmd_prefix};

mod flac;
pub use flac::{
    compress_audio_block, compress_audio_blocks, decompress_flac_block
};

mod lib_error_handling;
pub use lib_error_handling::SpriteShrinkCDError;

mod lib_structs;
pub use lib_structs::{
    MAX_AUDIO_BLOCK_SECTORS, MAX_DISC_SECTORS, NO_COLLECTION, SSMD_UID,
    CueSheet, ContentBlock, DataChunkLayout, DiscArea, DiscFile, DiscIndex,
    DiscManifest, DiscTrack, TrackType,
    ExceptionInfo, ExceptionRegistry, ExceptionTable, ExceptionType,
    DiscReconstructor, ParsedSSMD, RleSectorMap, SectorMap, SectorType,
    SSMDFormatData, SSMDIndices, SSMDTocEntry, SubHeaderEntry,
    SubheaderRegistry, SubheaderTable
};

mod mapper;
pub use mapper::{
    SectorDataProvider, analyze_and_map_disc, disc_files, rle_encode_map
};

mod reconstruction;
pub use reconstruction::{
    ReconstructionError,
    expand_exception_index, expand_sector_map, expand_subheader_map,
    rebuild_sector_bitwise, rebuild_sector_simd, verify_disc_integrity,
    write_disc
};

mod stream;
pub use stream::{
    MultiBinStream, SectorRegionStream, UserDataStream
};

mod util;
