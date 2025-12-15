use std::{
    array::TryFromSliceError,
    path::{Path, PathBuf},
};

use crc::{Crc, CRC_32_CD_ROM_EDC};
use thiserror::Error;

use crate::{
    lib_structs::{
        CueAnalysisResult, CueSheet, DiscValidationResult, SectorAnalysis,
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

const CRC_EDC_ALGORITHM: Crc<u32> = Crc::<u32>::new(&CRC_32_CD_ROM_EDC);

pub trait FileSizeProvider {
    fn get_file_size_bytes(
        &self, filename: &str
    ) -> Result<u64, std::io::Error>;
}

pub fn analyze_sector(
    sector: &[u8; 2352]
) -> Result<SectorAnalysis, AnalysisError> {
    const SYNC_BYTES: [u8;12] = [
      0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
      0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00
    ];

    if sector[0..12] == SYNC_BYTES{
        let mode = sector[15];
        match mode {
            // Mode 1: Standard data sectors.
            // Likely a PC Engine CD, Sega CD or a Sega Saturn disc sector
            1 => {
                let stored_edc_bytes: [u8; 4] = sector[2064..2068].try_into()?;
                let stored_checksum =  u32::from_le_bytes(stored_edc_bytes);
                let edc_check_data = &sector[12..2064];

                let calc_checksum = CRC_EDC_ALGORITHM.checksum(edc_check_data);

                if calc_checksum == stored_checksum {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode1,
                        user_data_range: 16..(16 + 2048),
                        meta_data: None
                    })
                } else {
                    let mut meta_data_vec = Vec::with_capacity(
                        16 + 2352 - (16 + 2048)
                    );
                    meta_data_vec.extend_from_slice(&sector[0..16]);
                    meta_data_vec.extend_from_slice(&sector[2064..]);

                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode1Exception,
                        user_data_range: 16..(16 + 2048),
                        meta_data: Some(meta_data_vec)
                    })
                }
            },
            // Mode 2: XA sectors.
            2 => {
                // All PS1 games and 'Blue Disc' PS2 games use Mode 2 disc
                // sectors but the form differs based on content e.g.
                // data (form1), audio or video (form2).
                let is_form2 = (sector[18] & (1 << 5)) != 0;

                if is_form2 {
                    Ok(SectorAnalysis {
                        sector_type: SectorType::Mode2Form2,
                        user_data_range: 24..(24 + 2324),
                        meta_data: None
                    })
                } else {
                    let stored_edc_bytes: [u8; 4] = sector[2072..2076].try_into()?;
                    let stored_checksum =  u32::from_le_bytes(
                        stored_edc_bytes
                    );
                    let edc_check_data = &sector[16..2072];

                    let calc_checksum = CRC_EDC_ALGORITHM.checksum(
                        edc_check_data
                    );
                    if calc_checksum == stored_checksum {
                        Ok(SectorAnalysis {
                            sector_type: SectorType::Mode2Form1,
                            user_data_range: 24..(24 + 2048),
                            meta_data: None
                        })
                    } else {
                        let mut meta_data_vec = Vec::with_capacity(
                            //Sync+header+Subheader+(EDC, ECC, Reserved)
                            16 + 8 + 2352 - (24 + 2048)
                        );
                        meta_data_vec.extend_from_slice(&sector[0..24]);
                        meta_data_vec.extend_from_slice(&sector[2072..]);

                        Ok(SectorAnalysis {
                            sector_type: SectorType::Mode2Form1Exception,
                            user_data_range: 24..(24 + 2048),
                            meta_data: Some(meta_data_vec),
                        })
                    }
                }
            }
            unknown_mode => {
                Err(AnalysisError::UnknownMode(unknown_mode))
            }
        }
    } else {
        //Audio sector
        Ok(SectorAnalysis {
            sector_type: SectorType::Audio,
            user_data_range: 0..2352,
            meta_data: None }
        )
    }
}


pub fn analyze_cue_sheets<F>(
    cue_sheets: &[(&Path, &str)],
    mut size_resolver: F,
) -> Result<CueAnalysisResult, SpriteShrinkCDError>
where
    F: FnMut(&Path) -> Result<u64, SpriteShrinkCDError>,
{
    let mut total_data_size = 0u64;
    let mut parsed_sheets: Vec<CueSheet> = Vec::with_capacity(
        cue_sheets.len()
    );


    for (cue_path, cue_content) in cue_sheets {
        let sheet = parse_cue(
            &cue_path.file_name().unwrap().to_string_lossy(),
            cue_content
        )?;

        let cue_dir = cue_path.parent().unwrap();

        for file_entry in &sheet.files {
            let data_file_path = cue_dir.join(&file_entry.name);

            total_data_size += size_resolver(&data_file_path)?;
        }

        parsed_sheets.push(sheet);
    }

    Ok(CueAnalysisResult {
        total_data_size,
        cue_sheets: parsed_sheets
    })
}


fn is_all_zeros(data: &[u8]) -> bool {
    data.iter().all(|&byte| byte == 0)
}


pub fn resolve_file_sector_counts(
    cue_sheet: &CueSheet,
    provider: &impl FileSizeProvider,
) -> Result<Vec<u32>, AnalysisError> {
    let mut sector_counts = Vec::with_capacity(cue_sheet.files.len());

    for file in &cue_sheet.files {
        match provider.get_file_size_bytes(&file.name) {
            Ok(size_in_bytes) => {
                if size_in_bytes % 2352 == 0 {
                    let sectors = (size_in_bytes / 2352) as u32;
                    sector_counts.push(sectors);
                } else {
                    return Err(AnalysisError::MalformedBin(file.name.clone()))
                }
            }
            Err(e) => {
                return Err(AnalysisError::FileSizeError (
                    file.name.clone(),
                    e,
                ));
            }
        }
    }
    Ok(sector_counts)
}


fn sector_check(
    sector: &[u8; 2352],
    crc_bytes: &[u8; 4]
) -> bool {
    let edc_calculator = Crc::<u32>::new(&CRC_32_CD_ROM_EDC);
    let checksum = edc_calculator.checksum(&sector[12..2064]);

    let crc_value: u32 = u32::from_le_bytes(*crc_bytes);

    if crc_value == checksum {
        true //Verification pass, no mismatch
    } else {
        false //Verification failed, data mismatch
    }
}

/// Validates sector 16 from a potential .bin file to help determine if it's a
/// valid disc image.
///
/// This function performs no I/O. The caller is responsible for seeking to
/// byte offset 37632 in the file, reading exactly 2352 bytes into an array,
/// and passing a reference to that array.
///
/// # Arguments
///
/// * `file_byte_size`: The total size of the original file in bytes.
/// * `sector_16_data`: A reference to a 2352-byte array read from the file,
///   starting at offset 37632.
///
/// # Returns
///
/// A `DiscValidationResult` struct containing the results of all performed
/// checks.
pub fn validate_cd_sector_16(
    file_byte_size: u64,
    sector_16_data: &[u8; 2352],
) -> DiscValidationResult {
    const ISO_PVD_OFFSET_REL: usize = 0;
    const ISO_PVD_MARKER: &[u8] = b"\x01CD001";

    const PS1_SYSTEM_OFFSET_REL: usize = 25;
    const PS1_SYSTEM_MARKER: &[u8] = b"CD001";

    const PS1_XA_OFFSET_REL: usize = 1048;
    const PS1_XA_MARKER: &[u8] = b"CD-XA001";

    let mut result = DiscValidationResult {
        is_multiple_of_2352: file_byte_size > 0 && file_byte_size.is_multiple_of(2352),
        has_iso_pvd_marker: false,
        has_ps1_system_marker: false,
        has_ps1_xa_marker: false,
    };

    if result.is_multiple_of_2352 {
        result.has_iso_pvd_marker = sector_16_data.get(
            ISO_PVD_OFFSET_REL..ISO_PVD_OFFSET_REL + ISO_PVD_MARKER.len()
        ) == Some(ISO_PVD_MARKER);

        result.has_ps1_system_marker = sector_16_data.get(
            PS1_SYSTEM_OFFSET_REL..PS1_SYSTEM_OFFSET_REL + PS1_SYSTEM_MARKER.len()
        ) == Some(PS1_SYSTEM_MARKER);

        result.has_ps1_xa_marker = sector_16_data.get(
            PS1_XA_OFFSET_REL..PS1_XA_OFFSET_REL + PS1_XA_MARKER.len()
        ) == Some(PS1_XA_MARKER);
    }

    result
}
