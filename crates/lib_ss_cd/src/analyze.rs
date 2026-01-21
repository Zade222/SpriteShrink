use std::{
    array::TryFromSliceError, io, path::{Path, PathBuf}
};

use thiserror::Error;

use crate::{
    ecc::{
        sector_ecc_check_bitwise, sector_edc_check
    },
    lib_structs::{
        CueAnalysisResult, CueSheet, SectorAnalysis,
        SectorType
    },
    lib_error_handling::SpriteShrinkCDError,
    cue_parser::parse_cue,
};

#[derive(Error, Debug)]
pub enum AnalysisError {
    #[error("Unknown sector type {0} found.")]
    UnknownMode(u8),
    #[error("Failed to convert slice to array for checksum: {0}")]
    SliceConversion(#[from] TryFromSliceError),
    #[error("Failed to get size for file '{0}': {1}")]
    FileSizeError(String, #[source] std::io::Error),
    #[error("{0} is a malformed bin file. Not a multiple of 2352 bytes.")]
    MalformedBin(String)
}

pub const SYNC_PATTERN: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00
];


pub trait FileSizeProvider {
    fn get_file_size_bytes(
        &self, filename: &str
    ) -> Result<u64, std::io::Error>;
}


pub fn analyze_data_sector(
    sector: &[u8; 2352],
    sector_type: SectorType
) -> Result<SectorAnalysis, AnalysisError> {
    let mode = sector[15];

    match mode {
        // Mode 1: Standard data sectors.
        // Likely a PC Engine CD, Sega CD or a Sega Saturn disc sector
        1 => {
            if sector_edc_check(
                sector,
                &sector[2064..2068],
                SectorType::Mode1
            ) && sector_ecc_check_bitwise(
                sector,
                &sector[2076..2248],
                &sector[2248..],
                SectorType::Mode1
            ) {
                if sector_type == SectorType::PregapMode1 {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::PregapMode1,
                        user_data_range: 16..2064,
                        exception_data: None
                    })
                } else {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode1,
                        user_data_range: 16..2064,
                        exception_data: None
                    })
                }
            } else {
                let mut meta_data_vec = Vec::with_capacity(296);
                //Store the following:
                // Sync bytes (0..12),
                // header bytes (12..16),
                // the EDC bytes (2064..2068),
                // ECC bytes (2076..)
                meta_data_vec.extend_from_slice(&sector[..16]);
                meta_data_vec.extend_from_slice(&sector[2064..2068]);
                //skip reserve, stored elsewhere
                meta_data_vec.extend_from_slice(&sector[2076..]);

                if sector_type == SectorType::PregapMode1 {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::PregapMode1Exception,
                        user_data_range: 16..2064,
                        exception_data: Some(meta_data_vec)
                    })
                } else {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode1Exception,
                        user_data_range: 16..2064,
                        exception_data: Some(meta_data_vec)
                    })
                }
            }
        },
        // Mode 2: XA sectors.
        2 => {
            // All PS1 games and 'Blue Disc' PS2 games use Mode 2 disc
            // sectors but the form differs based on content e.g.
            // data (form1), audio or video (form2).
            let is_form2 = (sector[18] & (1 << 5)) != 0;

            if is_form2 {
                if sector_edc_check(
                    sector,
                    &sector[2348..],
                    SectorType::Mode2Form2
                ) {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode2Form2,
                        user_data_range: 24..2348,
                        exception_data: None
                    })
                } else {
                    let mut meta_data_vec = Vec::with_capacity(20);
                    //Store the following:
                    // Sync bytes (0..12),
                    // header bytes (12..16),
                    // the EDC bytes (2348..2352),
                    meta_data_vec.extend_from_slice(&sector[..16]);
                    meta_data_vec.extend_from_slice(&sector[2348..]);

                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode2Form2Exception,
                        user_data_range: 24..2348,
                        exception_data: Some(meta_data_vec)
                    })
                }
            } else if sector_edc_check(
                sector,
                &sector[2072..2076],
                SectorType::Mode2Form1
            ) && sector_ecc_check_bitwise(
                sector,
                &sector[2076..2248],
                &sector[2248..],
                SectorType::Mode2Form1
            ) {
                if sector_type == SectorType::PregapMode2 {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::PregapMode2,
                        user_data_range: 24..2072,
                        exception_data: None
                    })
                } else {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode2Form1,
                        user_data_range: 24..2072,
                        exception_data: None
                    })
                }
            } else { //likely libcrypt but could be a bad rip
                let mut meta_data_vec = Vec::with_capacity(296);
                //Store the following:
                // Sync bytes (0..12),
                // header bytes (12..16),
                // the EDC bytes (2072..2076),
                // ECC bytes (2076..)
                meta_data_vec.extend_from_slice(&sector[..16]);
                meta_data_vec.extend_from_slice(&sector[2072..]);

                if sector_type == SectorType::PregapMode2 {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::PregapMode2Exception,
                        user_data_range: 24..2072,
                        exception_data: Some(meta_data_vec),
                    })
                } else {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode2Form1Exception,
                        user_data_range: 24..2072,
                        exception_data: Some(meta_data_vec),
                    })
                }
            }
        }
        0 =>{
            if *sector == [0u8; 2352] {
                let zero_type = match sector_type {
                    SectorType::Mode2Form1 |
                    SectorType::Mode2Form2 |
                    SectorType::PregapMode2 => SectorType::ZeroedMode2Data,

                    _ => SectorType::ZeroedMode1Data,
                };


                Ok(SectorAnalysis {
                    sector_type: zero_type,
                    user_data_range: 0..2352,
                    exception_data: None,
                })
            } else {
                Err(AnalysisError::UnknownMode(0))
            }
        }
        unknown_mode => {
            Err(AnalysisError::UnknownMode(unknown_mode))
        }
    }
}


pub fn analyze_cue_sheets<F>(
    cue_sheets: &[(PathBuf, String)],
    size_resolver: F,
) -> Result<CueAnalysisResult, SpriteShrinkCDError>
where
    F: Fn(&Path) -> Result<u64, io::Error>,
{
    let mut total_data_size = 0u64;
    let mut parsed_sheets: Vec<CueSheet> = Vec::with_capacity(
        cue_sheets.len()
    );

    for (cue_path, cue_content) in cue_sheets {
        let sheet = parse_cue(
            &cue_path.file_stem().unwrap().to_string_lossy(),
            cue_content
        )?;

        let cue_dir = cue_path.parent().unwrap();

        for file_entry in &sheet.files {
            let data_file_path = cue_dir.join(&file_entry.name);

            let size = size_resolver(&data_file_path)?;
            total_data_size += size;
        }

        parsed_sheets.push(sheet);
    }

    Ok(CueAnalysisResult {
        total_data_size,
        cue_sheets: parsed_sheets
    })
}


pub fn resolve_file_sector_counts(
    cue_sheet: &CueSheet,
    provider: &impl FileSizeProvider,
) -> Result<Vec<u32>, SpriteShrinkCDError> {
    let mut sector_counts = Vec::with_capacity(cue_sheet.files.len());

    for file in &cue_sheet.files {
        match provider.get_file_size_bytes(&file.name) {
            Ok(size_in_bytes) => {
                if size_in_bytes % 2352 == 0 {
                    let sectors = (size_in_bytes / 2352) as u32;
                    sector_counts.push(sectors);
                } else {
                    return Err(AnalysisError::MalformedBin(file.name.clone())
                        .into()
                    )
                }
            }
            Err(e) => {
                return Err(AnalysisError::FileSizeError(
                    file.name.clone(),
                    e,
                ).into());
            }
        }
    }
    Ok(sector_counts)
}
