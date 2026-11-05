use std::{
    borrow::Cow,
    io::{self, Read, Seek},
};

use sprite_shrink::{
    Hashable
};

use crate::lib_error_handling::SpriteShrinkCDError;

use crate::lib_structs::{
    ContentBlock, CueFile, CueSheet, ExceptionRegistry, ExceptionType, MsfTime,
    RleSectorMap, SectorMap, SectorMapResult, SectorType,
    SubHeaderEntry, SubheaderRegistry, Track, TrackType
};

use crate::{
    analyze::analyze_data_sector,
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
            SectorType::PregapAudio => {
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


pub fn analyze_and_map_disc(
    cue_sheet: &CueSheet,
    file_sec_count: &[u32],
    provider: &mut impl SectorDataProvider,
    subheader_registry: &SubheaderRegistry,
    exception_registry: &ExceptionRegistry
) -> Result<SectorMapResult, SpriteShrinkCDError> {
    let normalized_sheet= if cue_sheet.files.len() > 1 {
        Cow::Owned(normalize_cue_sheet(
            cue_sheet,
            file_sec_count,
            &cue_sheet.source_filename
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
        let content_start = get_track_content_start(track)?;
        let next_track_start = match tracks_iter.peek() {
            Some(next_track) => get_track_start_sector(next_track)?,
            None => total_sectors,
        };

        let coarse_pregap_type = match track.track_type {
            TrackType::Audio => SectorType::PregapAudio,
            TrackType::Mode1_2352 => SectorType::PregapMode1,
            TrackType::Mode2_2352 => SectorType::PregapMode2,
        };

        for i in track_start_sector..content_start {
            if let Some(sector) = sectors.get_mut(i as usize) {
                *sector = coarse_pregap_type;
            }
        }

        let coarse_type = match track.track_type {
            TrackType::Audio => SectorType::Audio,
            TrackType::Mode1_2352 => SectorType::Mode1,
            TrackType::Mode2_2352 => SectorType::Mode2Form1
        };

        for i in content_start..next_track_start {
            if let Some(sector) = sectors.get_mut(i as usize) {
                *sector = coarse_type;
            }
        }
    }

    let mut subheader_index: Vec<SubHeaderEntry> = Vec::new();

    let mut current_subheader_id: Option<u16> = None;

    let mut current_sub_count = 0;
    let mut current_run_start = 0;
    let mut lba_map: Vec<(u32, u32)> = Vec::new();
    let mut current_offset: Option<u32> = None;
    let mut excep_index: Vec<(u32, u32)> = Vec::new();

    for i in 0..total_sectors {
        let sector_data = provider.read_sector(i)?;
        if sectors[i as usize] == SectorType::Mode1 ||
            sectors[i as usize] == SectorType::Mode2Form1 ||
            sectors[i as usize] == SectorType::PregapMode1 ||
            sectors[i as usize] == SectorType::PregapMode2
        {
            let analysis_result = analyze_data_sector(
                &sector_data,
                sectors[i as usize]
            )?;

            let minute = from_bcd(sector_data[12]);
            let second = from_bcd(sector_data[13]);
            let frame = from_bcd(sector_data[14]);

            let header_lba = (minute as u32 * 60 * 75) +
                (second as u32 * 75) + frame as u32;

            if header_lba >= i {
                let detected_offset = header_lba - i;

                if let Some(curr) = current_offset {
                    if curr != detected_offset {
                        //offset changed (likely GD-ROM gap)
                        lba_map.push((i, detected_offset));
                        current_offset = Some(detected_offset);
                    }
                } else {
                    lba_map.push((i, detected_offset));
                    current_offset = Some(detected_offset);
                }
            }


            let subheader_val = if matches!(
                analysis_result.sector_type,
                SectorType::Mode2Form1 | SectorType::Mode2Form2 |
                SectorType::Mode2Form1Exception |
                SectorType::Mode2Form2Exception |
                SectorType::PregapMode1Exception |
                SectorType::PregapMode2Exception
            ) {
                let mut sh = [0u8; 8];
                sh.copy_from_slice(&sector_data[16..24]);
                Some(sh)
            } else {
                let mut sh = [0u8; 8];
                sh.copy_from_slice(&sector_data[2068..2076]);
                Some(sh)
            };

            sectors[i as usize] = analysis_result.sector_type;

            if let Some(val) = subheader_val {
                let val_id = subheader_registry.get_or_register(val);

                if let Some(curr_id) = current_subheader_id {
                    if curr_id == val_id {
                        if i == (current_run_start + current_sub_count) {
                            current_sub_count += 1;
                        } else {
                            subheader_index.push(SubHeaderEntry {
                                start_lba: current_run_start,
                                count: current_sub_count,
                                data_id: curr_id
                            });
                            current_sub_count = 1;
                            current_run_start = i;
                        }
                    } else {
                        subheader_index.push(SubHeaderEntry {
                            start_lba: current_run_start,
                            count: current_sub_count,
                            data_id: curr_id
                        });
                        current_subheader_id = Some(val_id);
                        current_sub_count = 1;
                        current_run_start = i;
                    }
                } else {
                    current_subheader_id = Some(val_id);
                    current_sub_count = 1;
                    current_run_start = i;
                }
            }


            if let Some(data) = analysis_result.exception_data {
                let excep_type = match analysis_result.sector_type {
                    SectorType::Mode1Exception => ExceptionType::Mode1,
                    SectorType::Mode2Form1Exception => ExceptionType::Mode2Form1,
                    SectorType::Mode2Form2Exception => ExceptionType::Mode2Form2,
                    _ => ExceptionType::None
                };

                if excep_type != ExceptionType::None {
                    let id = exception_registry.get_or_register(data);
                    excep_index.push((i, id));
                };
            }
        }
    }

    if let Some(curr_id) = current_subheader_id {
        subheader_index.push(SubHeaderEntry {
            start_lba: current_run_start,
            count: current_sub_count,
            data_id: curr_id
        });
    }

    for sector in sectors.iter_mut() {
        if *sector == SectorType::None {
            *sector = SectorType::PregapAudio;
        }
    }

    Ok(SectorMapResult{
        sector_map: SectorMap { sectors },
        exception_index: excep_index,
        subheader_index,
        lba_map
    })
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

    let mut normalized_tracks = Vec::new();
    let mut file_start_offset: u32 = 0;

    for (file_idx, file) in source_sheet.files.iter().enumerate() {
        for track in &file.tracks {
            let mut new_track = track.clone();
            new_track.indices.clear();

            let offset = file_start_offset;

            for index in &track.indices {
                let relative_sectors = index.position.to_total_frames();
                let absolute_sectors = offset + relative_sectors;

                let mut new_index = index.clone();
                new_index.position = MsfTime::from_total_frames(absolute_sectors);
                new_track.indices.push(new_index);
            }
            normalized_tracks.push(new_track);
        }

        file_start_offset += file_sec_count[file_idx];
    }

    Ok(CueSheet {
        source_filename: new_bin_filename.to_string(),
        files: vec![CueFile {
            name: new_bin_filename.to_string(),
            tracks: normalized_tracks,
        }],
    })
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

pub(crate) const fn from_bcd (bcd: u8) -> u8 {
    let tens = (bcd >> 4) * 10;
    let ones = bcd & 0x0F;
    tens + ones
}


#[cfg(test)]
mod tests {
    use std::iter::repeat_n;

    use proptest::prelude::*;

    use super::*;
    use crate::reconstruction::expand_sector_map;

    const ALL_SECTOR_TYPES: [SectorType; 16] = [
        SectorType::Audio,
        SectorType::Mode1,
        SectorType::Mode1Exception,
        SectorType::Mode2Form1,
        SectorType::Mode2Form1Exception,
        SectorType::Mode2Form2,
        SectorType::Mode2Form2Exception,
        SectorType::PregapMode1,
        SectorType::PregapMode1Exception,
        SectorType::PregapMode2,
        SectorType::PregapMode2Exception,
        SectorType::PregapAudio,
        SectorType::ZeroedAudio,
        SectorType::ZeroedMode1Data,
        SectorType::ZeroedMode2Data,
        SectorType::None,
    ];

    /// A sector map built from runs, so long repeats are common. Adjacent
    /// runs can share a type, which `rle_encode_map` has to merge.
    fn sector_map() -> impl Strategy<Value = Vec<SectorType>> {
        proptest::collection::vec(
            (proptest::sample::select(ALL_SECTOR_TYPES.to_vec()), 1..40_usize),
            0..20,
        ).prop_map(|runs| {
            runs.into_iter()
                .flat_map(|(sector_type, count)| repeat_n(sector_type, count))
                .collect()
        })
    }

    fn encode(sectors: &[SectorType]) -> RleSectorMap {
        rle_encode_map(&SectorMap { sectors: sectors.to_vec() })
    }

    proptest! {
        #[test]
        fn expanding_an_encoded_map_gives_it_back(sectors in sector_map()) {
            prop_assert_eq!(expand_sector_map(&encode(&sectors)), sectors);
        }

        #[test]
        fn encoded_runs_are_as_long_as_possible(sectors in sector_map()) {
            let rle = encode(&sectors);
            prop_assert!(rle.runs.iter().all(|&(count, _)| count > 0));
            prop_assert!(rle.runs.windows(2).all(|pair| pair[0].1 != pair[1].1));
        }
    }

    #[test]
    fn a_map_without_repeats_encodes_as_runs_of_one() {
        let rle = encode(&ALL_SECTOR_TYPES);
        assert_eq!(rle.runs.len(), ALL_SECTOR_TYPES.len());
        for (&(count, sector_type), expected) in rle.runs.iter().zip(ALL_SECTOR_TYPES) {
            assert_eq!((count, sector_type), (1, expected));
        }
    }

    #[test]
    fn an_empty_map_encodes_as_no_runs() {
        assert_eq!(encode(&[]).runs.len(), 0);
    }
}
