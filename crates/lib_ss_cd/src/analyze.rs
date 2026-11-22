use std::{
    array::TryFromSliceError, io, path::{Path, PathBuf}
};

use thiserror::Error;

use crate::{
    lib_structs::{
        MAX_DISC_SECTORS,
        CueAnalysisResult, CueSheet, SectorAnalysis,
        SectorType
    },
    lib_error_handling::SpriteShrinkCDError,
    cue_parser::parse_cue,
    reconstruction::rebuild_sector_simd,
};

#[derive(Error, Debug)]
pub enum AnalysisError {
    #[error("Failed to convert slice to array for checksum: {0}")]
    SliceConversion(#[from] TryFromSliceError),
    #[error("Failed to get size for file '{0}': {1}")]
    FileSizeError(String, #[source] std::io::Error),
    #[error("{0} is a malformed bin file. Not a multiple of 2352 bytes.")]
    MalformedBin(String),
    #[error("The disc's files hold {0} or more sectors, more than the \
        {max} a CD or GD-ROM can hold.", max = MAX_DISC_SECTORS)]
    TooManySectors(u64),
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


/// The Mode 2 subheader bit (submode, byte 18) that marks a Form 2 sector.
const FORM2_BIT: u8 = 1 << 5;

/// Decides how a data sector is stored.
///
/// A sector is stored plain (Mode 1, Mode 2 Form 1 or Form 2) only if
/// rebuilding it as extraction does gives back all 2352 bytes: from its user
/// data, its subheader or reserved bytes, and `sector_num`, the number
/// extraction will put in its header. Anything else is stored as an
/// exception. The EDC and ECC checks this replaces didn't cover a Mode 2
/// sector's sync and header, so a sector could pass them and come back
/// different.
///
/// An exception keeps every byte outside the user data and the subheader or
/// reserved bytes, which are stored too, so it always rebuilds exactly.
/// A sector of no mode a disc defines (Mode 0 with data, or a mode byte
/// above 2) is stored as a Mode 1 exception for that reason.
///
/// # Arguments
///
/// * `sector`: The raw sector.
/// * `sector_type`: The track's type at this sector, as the mapper gives
///   it: `Mode1`, `Mode2Form1`, or a pregap type.
/// * `sector_num`: The number extraction will give this sector's header,
///   or `None` if no header so far has given one, in which case the sector
///   is stored as an exception.
pub fn analyze_data_sector(
    sector: &[u8; 2352],
    sector_type: SectorType,
    sector_num: Option<u32>,
) -> SectorAnalysis {
    let (plain, exception) = match sector[15] {
        0 if sector.iter().all(|&byte| byte == 0) => {
            let zeroed = match sector_type {
                SectorType::Mode2Form1 |
                SectorType::Mode2Form2 |
                SectorType::PregapMode2 => SectorType::ZeroedMode2Data,
                _ => SectorType::ZeroedMode1Data,
            };
            return SectorAnalysis { sector_type: zeroed, exception_data: None };
        }
        1 if sector_type == SectorType::PregapMode1 =>
            (SectorType::PregapMode1, SectorType::PregapMode1Exception),
        1 => (SectorType::Mode1, SectorType::Mode1Exception),
        // Form 2 sectors hold audio or video, Form 1 sectors data.
        2 if sector[18] & FORM2_BIT != 0 =>
            (SectorType::Mode2Form2, SectorType::Mode2Form2Exception),
        2 if sector_type == SectorType::PregapMode2 =>
            (SectorType::PregapMode2, SectorType::PregapMode2Exception),
        2 => (SectorType::Mode2Form1, SectorType::Mode2Form1Exception),
        _ if sector_type.is_pregap() =>
            return as_exception(sector, SectorType::PregapMode1Exception),
        _ => return as_exception(sector, SectorType::Mode1Exception),
    };

    if sector_num.is_some_and(|number| rebuilds_exactly(sector, plain, number)) {
        SectorAnalysis { sector_type: plain, exception_data: None }
    } else {
        as_exception(sector, exception)
    }
}


/// Whether rebuilding `sector` as a plain `sector_type` sector numbered
/// `sector_num`, from the bytes the archive stores for it, gives it back.
fn rebuilds_exactly(
    sector: &[u8; 2352],
    sector_type: SectorType,
    sector_num: u32,
) -> bool {
    let Some(user_data) = sector_type.user_data_range()
        .and_then(|range| sector.get(range))
    else {
        return false;
    };
    let Some(metadata) = sector.get(sector_type.metadata_range())
        .and_then(|bytes| <&[u8; 8]>::try_from(bytes).ok())
    else {
        return false;
    };

    rebuild_sector_simd(user_data, metadata, sector_type, sector_num, None)
        .is_ok_and(|rebuilt| rebuilt == *sector)
}


/// `sector` as an exception of type `exception`: the bytes outside its
/// user data and subheader or reserved bytes, in the order the rebuild
/// reads them.
fn as_exception(sector: &[u8; 2352], exception: SectorType) -> SectorAnalysis {
    let mut kept = Vec::with_capacity(exception.excep_size());
    //Sync and header
    kept.extend_from_slice(&sector[..16]);
    match exception {
        //EDC, then ECC after the reserved bytes
        SectorType::Mode1Exception | SectorType::PregapMode1Exception => {
            kept.extend_from_slice(&sector[2064..2068]);
            kept.extend_from_slice(&sector[2076..]);
        }
        //EDC
        SectorType::Mode2Form2Exception => kept.extend_from_slice(&sector[2348..]),
        //EDC and ECC
        _ => kept.extend_from_slice(&sector[2072..]),
    }

    SectorAnalysis { sector_type: exception, exception_data: Some(kept) }
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

            let size = size_resolver(&data_file_path).map_err(|err| {
                AnalysisError::FileSizeError(
                    data_file_path.display().to_string(),
                    err,
                )
            })?;
            /*Only used to decide whether the input fits in memory, which a
            total past u64::MAX doesn't.*/
            total_data_size = total_data_size.saturating_add(size);
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
    let mut total_sectors = 0_u64;

    for file in &cue_sheet.files {
        match provider.get_file_size_bytes(&file.name) {
            Ok(size_in_bytes) => {
                if size_in_bytes % 2352 == 0 {
                    let sectors = size_in_bytes / 2352;
                    total_sectors = total_sectors.saturating_add(sectors);
                    if total_sectors > MAX_DISC_SECTORS {
                        return Err(
                            AnalysisError::TooManySectors(total_sectors).into()
                        );
                    }
                    /*The cap is far below u32::MAX, so this always fits.*/
                    let Ok(sectors) = u32::try_from(sectors) else {
                        return Err(
                            AnalysisError::TooManySectors(total_sectors).into()
                        );
                    };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file is `sectors` sectors long.
    struct Sectors(u64);

    impl FileSizeProvider for Sectors {
        fn get_file_size_bytes(&self, _: &str) -> Result<u64, io::Error> {
            Ok(self.0 * 2352)
        }
    }

    #[test]
    fn a_disc_past_the_sector_cap_is_rejected_across_its_files() {
        let sheet = parse_cue(
            "disc",
            "FILE \"a.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n\
             FILE \"b.bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        // Two files of half the cap each: exactly at the cap.
        let half = MAX_DISC_SECTORS / 2;
        assert_eq!(
            resolve_file_sector_counts(&sheet, &Sectors(half)).unwrap(),
            [u32::try_from(half).unwrap(); 2]
        );

        let result = resolve_file_sector_counts(&sheet, &Sectors(half + 1));
        assert!(
            matches!(
                result,
                Err(SpriteShrinkCDError::Analysis(
                    AnalysisError::TooManySectors(_)
                ))
            ),
            "{result:?}"
        );
    }
}
