use std::borrow::Cow;
use std::io::{self, Read, Seek};
use std::iter::repeat_n;

use sprite_shrink::{
    Hashable
};

use crate::lib_structs::{
    ContentBlock, CueFile, CueSheet, CueSheetType, MsfTime, RleSectorMap,
    SectorMap, SectorType, Track, TrackType
};

use crate::{
    analyze::analyze_sector,
    stream::SectorRegionStream
};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MapperError {
    #[error("Track {0} is missing an INDEX 01 entry.")]
    MissingIndex01(u8),
    #[error("The CUE sheet is empty and contains no files to map.")]
    EmptyCueSheet,
    #[error("The number of file lengths provided ({0}) does not match the number of files in the CUE sheet ({1}).")]
    FileCountMismatch(usize, usize),
    #[error("Failed to read from sector provider: {0}")]
    ProviderIo(#[from] io::Error),
    #[error("Failed to analyze sector data: {0}")]
    Analysis(#[from] crate::analyze::AnalysisError),
}

pub trait SectorDataProvider {
    fn read_sector(&mut self, sector_index: u32) -> io::Result<[u8; 2352]>;
}


pub fn build_block_map<H, R>(
    rle_sector_map: &RleSectorMap,
    source: &mut R,
) -> Result<Vec<ContentBlock<H>>, io::Error>
where
    H: Hashable,
    R: Read + Seek,
{
    let mut block_map = Vec::new();
    let mut absolute_sector_offset = 0;

    const AUDIO_SEGMENT_SIZE: u32 = 16;

    for (run_count, sector_type) in &rle_sector_map.runs {
        match sector_type {
            SectorType::Audio => {
                let mut sectors_in_run_processed = 0;
                while sectors_in_run_processed < *run_count {
                    let segment_start =
                        absolute_sector_offset + sectors_in_run_processed;
                    let segment_size = std::cmp::min(
                        AUDIO_SEGMENT_SIZE,
                        *run_count - sectors_in_run_processed
                    );

                    let mut stream = SectorRegionStream::new(
                        source,
                        segment_start,
                        segment_size
                    );

                    let mut buffer = Vec::new();
                    stream.read_to_end(&mut buffer)?;
                    let content_hash = H::from_bytes_with_seed(&buffer);

                    block_map.push(ContentBlock {
                        start_sector: segment_start,
                        sector_count: segment_size,
                        content_hash,
                        sector_type: *sector_type,
                    });

                    sectors_in_run_processed += segment_size;
                }
            }
            SectorType::Pregap => {
                let mut stream = SectorRegionStream::new(
                    source,
                    absolute_sector_offset,
                    *run_count
                );
                let mut buffer = Vec::new();
                stream.read_to_end(&mut buffer)?;
                let content_hash = H::from_bytes_with_seed(&buffer);

                block_map.push(ContentBlock {
                    start_sector: absolute_sector_offset,
                    sector_count: *run_count,
                    content_hash,
                    sector_type: *sector_type,
                });
            }
            // Ignore all others.
            _ => {}
        }

        absolute_sector_offset += run_count;
    }

    Ok(block_map)
}


pub fn build_sector_map(
    cue_sheet: &CueSheet,
    file_sec_count: &[u32],
    provider: &mut impl SectorDataProvider,
) -> Result<SectorMap, MapperError> {
    let mut normalized_sheet= if cue_sheet.files.len() > 1 {
        Cow::Owned(normalize_cue_sheet(
            cue_sheet,
            file_sec_count,
            cue_sheet.source_filename.as_str()
        )?)
    } else {
        Cow::Borrowed(cue_sheet)
    };

    let total_sectors = file_sec_count.iter().sum::<u32>();

    let mut sectors = vec![SectorType::None; total_sectors as usize];
    let tracks = &normalized_sheet.files
        .first()
        .ok_or(MapperError::EmptyCueSheet)?
        .tracks;
    let mut tracks_iter = tracks.iter().peekable();

    while let Some(track) = tracks_iter.next() {
        let track_start_sector = get_track_start_sector(track)?;
        let track_end_sector = match tracks_iter.peek() {
            Some(next_track) => get_track_start_sector(next_track)?,
            None => total_sectors,
        };

        let coarse_type = match track.track_type {
            TrackType::Audio => SectorType::Audio,
            _ => SectorType::Mode1,
        };

        for i in track_start_sector..track_end_sector {
            if let Some(sector) = sectors.get_mut(i as usize) {
                *sector = coarse_type;
            }
        }
    }

    for i in 0..total_sectors {
        if sectors[i as usize] == SectorType::Mode1 {
            let sector_data = provider.read_sector(i)?;
            let analysis_result = analyze_sector(&sector_data)?;
            sectors[i as usize] = analysis_result.sector_type;
        }
    }

    for sector in sectors.iter_mut() {
        if *sector == SectorType::None {
            *sector = SectorType::Pregap;
        }
    }

    Ok(SectorMap { sectors })
}


fn detect_cue_type(cue_sheet: &CueSheet) -> CueSheetType {
    let mut last_sector = 0;
    for file in &cue_sheet.files {
        for track in &file.tracks {
            if let Ok(content_start) = get_track_content_start(track) {
                if content_start < last_sector {
                    return CueSheetType::Relative;
                }
                last_sector = content_start;
            }
        }
    }
    CueSheetType::Absolute
}


fn get_track_content_start(track: &Track) -> Result<u32, MapperError> {
    track.indices
        .iter()
        .find(|i| i.number == 1)
        .map(|i| (i.position).to_total_frames())
        .ok_or(MapperError::MissingIndex01(track.number))
}

fn get_track_start_sector(track: &Track) -> Result<u32, MapperError> {
    let index_01 = track.indices.iter()
        .find(|i| i.number == 1)
        .map(|i| i.position.to_total_frames())
        .ok_or(MapperError::MissingIndex01(track.number))?;

    let index_00 = track.indices.iter()
        .find(|i| i.number == 0)
        .map(|i| i.position.to_total_frames());

    Ok(index_00.unwrap_or(index_01))
}


fn normalize_cue_sheet(
    source_sheet: &CueSheet,
    file_sec_count: &[u32],
    new_bin_filename: &str,
) -> Result<CueSheet, MapperError> {
    if file_sec_count.len() != source_sheet.files.len() {
        return Err(MapperError::FileCountMismatch(
            file_sec_count.len(),
            source_sheet.files.len()
        ))
    }

    let cue_type = detect_cue_type(source_sheet);
    let mut normalized_tracks = Vec::new();
    let mut file_start_offset: u32 = 0;

    for (file_idx, file) in source_sheet.files.iter().enumerate() {
        for track in &file.tracks {
            let mut new_track = track.clone();
            new_track.indices.clear();

            let offset = if cue_type == CueSheetType::Relative {
                file_start_offset
            } else {
                0
            };

            for index in &track.indices {
                let relative_sectors = index.position.to_total_frames();
                let absolute_sectors = offset + relative_sectors;

                let mut new_index = index.clone();
                new_index.position = MsfTime::from_total_frames(absolute_sectors);
                new_track.indices.push(new_index);
            }
            normalized_tracks.push(new_track);
        }

        if cue_type == CueSheetType::Relative {
            file_start_offset += file_sec_count[file_idx];
        }
    }

    Ok(CueSheet {
        source_filename: new_bin_filename.to_string(),
        files: vec![CueFile {
            name: new_bin_filename.to_string(),
            tracks: normalized_tracks,
        }],
    })
}

/// Decompresses a run-length encoded sector map into a full, per-sector vector
/// for fast lookups.
///
/// This function takes an `RleSectorMap` (which is optimized for compact
/// storage) and expands it into a `SectorMap` (which is optimized for fast,
/// O(1) lookups in memory).
///
/// # Arguments
/// - `rle_map`: A reference to the `RleSectorMap` to be decoded.
///
/// # Returns
/// A `SectorMap` containing a `Vec<SectorType>` where each element corresponds
/// to the type of an individual sector on the disc.
pub fn rle_decode_map(rle_map: &RleSectorMap) -> SectorMap {
    let total_sectors: usize = rle_map.runs
        .iter()
        .map(|(count, _)| *count as usize)
        .sum();

    let mut sectors = Vec::with_capacity(total_sectors);

    for (count, sector_type) in &rle_map.runs {
        sectors.extend(repeat_n(*sector_type, *count as usize));
    }

    SectorMap { sectors }
}


pub fn rle_encode_map(sector_map: &SectorMap) -> RleSectorMap {
    let mut runs: Vec<(u32, SectorType)> = Vec::new();

    if sector_map.sectors.is_empty() {
        return RleSectorMap { runs };
    }

    let mut current_type = sector_map.sectors[0];
    let mut current_count = 0;

    for &sector_type in sector_map.sectors.iter() {
        if sector_type == current_type {
            current_count += 1;
        } else {
            runs.push((current_count, current_type));
            current_type = sector_type;
            current_count = 1;
        }
    }
    runs.push((current_count, current_type));

    RleSectorMap { runs }
}
