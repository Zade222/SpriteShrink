use std::fmt::Write;

use sprite_shrink::Hashable;

use crate::lib_structs::{DiscManifest, MsfTime, TrackMode};

/// Writes the CUE sheet for a disc extracted to the single file
/// `bin_filename`.
///
/// The tracks and indices come from the manifest's file layout, moved to
/// their positions in the joined file. Archives written before format
/// version 2.1 have no layout, and their tracks are inferred from the
/// sector map instead (see `infer_cue_string`).
///
/// # Errors
///
/// Only if writing to the `String` fails, which it doesn't.
pub fn generate_cue_string<H: Hashable>(
    manifest: &DiscManifest<H>,
    bin_filename: &str,
) -> Result<String, std::fmt::Error> {
    if manifest.files.is_empty() {
        return infer_cue_string(manifest, bin_filename);
    }

    let mut output = String::new();
    writeln!(output, "FILE \"{bin_filename}\" BINARY")?;

    //Where each file starts in the joined file.
    let mut file_start: u32 = 0;
    for file in &manifest.files {
        for track in &file.tracks {
            writeln!(
                output,
                "  TRACK {:02} {}",
                track.number,
                track.track_type
            )?;
            for index in &track.indices {
                let sector = file_start.saturating_add(index.sector);
                writeln!(
                    output,
                    "    INDEX {:02} {}",
                    index.number,
                    MsfTime::from_total_frames(sector)
                )?;
            }
        }
        file_start = file_start.saturating_add(file.sector_count);
    }

    Ok(output)
}

/// Infers a CUE sheet from the sector map, for archives without a file
/// layout.
///
/// A track starts wherever the sector map's mode (audio, Mode 1, Mode 2)
/// changes or a pregap run begins. The sector map doesn't record track
/// starts, so this can be wrong: audio tracks with no pregap between them
/// merge into one; a data pregap that analysis split into several runs
/// gives several tracks; and a blank data pregap is typed as zeroed data,
/// so its INDEX 00 is lost.
fn infer_cue_string<H: Hashable>(
    manifest: &DiscManifest<H>,
    bin_filename: &str,
) -> Result<String, std::fmt::Error> {
    let mut output = String::new();
    writeln!(output, "FILE \"{}\" BINARY", bin_filename)?;

    let mut current_frame_count: u32 = 0;
    let mut current_track_number: u8 = 0;
    let mut last_track_mode: Option<TrackMode> = None;

    let mut pending_index_01 = false;

    for (run_length, sector_type) in &manifest.rle_sector_map.runs {
        let current_mode_opt = TrackMode::from_sector_type(*sector_type);
        let is_explicit_pregap = sector_type.is_pregap();

        if current_mode_opt.is_none() {
            current_frame_count += run_length;
            continue;
        }
        let mode = current_mode_opt.unwrap();

        let mode_changed = last_track_mode != Some(mode);

        let start_new_track = current_track_number == 0 || mode_changed || is_explicit_pregap;

        if start_new_track {
            current_track_number += 1;
            last_track_mode = Some(mode);

            writeln!(
                output,
                "  TRACK {:02} {}",
                current_track_number,
                mode.to_cue_type_string()
            )?;

            let time = MsfTime::from_total_frames(current_frame_count);

            if is_explicit_pregap {
                writeln!(output, "    INDEX 00 {}", time)?;
                pending_index_01 = true;
            } else {
                writeln!(output, "    INDEX 01 {}", time)?;
                pending_index_01 = false;
            }
        } else if pending_index_01 {
            let time = MsfTime::from_total_frames(current_frame_count);
            writeln!(output, "    INDEX 01 {}", time)?;
            pending_index_01 = false;
        }

        current_frame_count += run_length;
    }

    Ok(output)
}


#[cfg(test)]
mod tests {
    use crate::{
        cue_parser::parse_cue,
        lib_structs::{CueSheet, DiscFile, RleSectorMap, SectorType},
        mapper::{MapperError, disc_files},
    };

    use super::*;

    fn manifest(files: Vec<DiscFile>, runs: &[(u32, SectorType)])
        -> DiscManifest<u64>
    {
        DiscManifest {
            files,
            lba_map: Vec::new(),
            rle_sector_map: RleSectorMap { runs: runs.to_vec() },
            audio_block_map: Vec::new(),
            data_stream_layout: Vec::new(),
            subheader_index: Vec::new(),
            disc_exception_index: Vec::new(),
            integrity_hash: 0,
        }
    }

    /// The CUE sheet for a manifest with no file layout, as an archive
    /// from before format 2.1 has: inferred from `runs`.
    fn inferred(runs: &[(u32, SectorType)]) -> String {
        generate_cue_string(&manifest(Vec::new(), runs), "game.bin").unwrap()
    }

    /// Parses `content` and stores its layout, with each file
    /// `file_sectors` long, in a manifest whose sector map is `runs`.
    /// Returns the sheet and the CUE generated for the joined file.
    fn from_layout(
        content: &str,
        file_sectors: &[u32],
        runs: &[(u32, SectorType)],
    ) -> (CueSheet, String) {
        let sheet = parse_cue("game.cue", content).unwrap();
        let files = disc_files(&sheet, file_sectors).unwrap();
        let generated =
            generate_cue_string(&manifest(files, runs), "game.bin").unwrap();
        (sheet, generated)
    }

    /// Asserts that `generated` parses to the same sheet as `expected`.
    #[track_caller]
    fn assert_same_sheet(generated: &str, expected: &str) {
        assert_eq!(
            parse_cue("game.cue", generated).unwrap(),
            parse_cue("game.cue", expected).unwrap(),
            "generated:\n{generated}"
        );
    }

    #[test]
    fn a_layout_is_printed_for_the_joined_file() {
        let (_, generated) = from_layout("\
FILE \"disc.bin\" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:40:00
    INDEX 01 00:42:00
", &[6000], &[]);
        assert_eq!(
            generated,
            "FILE \"game.bin\" BINARY\n\
            \x20 TRACK 01 MODE2/2352\n\
            \x20   INDEX 01 00:00:00\n\
            \x20 TRACK 02 AUDIO\n\
            \x20   INDEX 00 00:40:00\n\
            \x20   INDEX 01 00:42:00\n"
        );
    }

    #[test]
    fn each_file_s_indices_move_to_where_the_file_starts() {
        // Track 2's file starts after track 1's 3000 sectors (00:40:00).
        let (_, generated) = from_layout("\
FILE \"game (Track 1).bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE \"game (Track 2).bin\" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
", &[3000, 4500], &[]);
        assert_same_sheet(&generated, "\
FILE \"game.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:40:00
    INDEX 01 00:42:00
");
    }

    #[test]
    fn the_layout_keeps_each_file_s_name_and_length() {
        let sheet = parse_cue("game.cue", "\
FILE \"a.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE \"b.bin\" BINARY
  TRACK 02 AUDIO
    INDEX 01 00:02:00
").unwrap();
        let files = disc_files(&sheet, &[3000, 4500]).unwrap();
        let summary: Vec<(&str, u32, Vec<u32>)> = files.iter()
            .map(|file| (
                file.name.as_str(),
                file.sector_count,
                file.tracks.iter()
                    .flat_map(|track| track.indices.iter().map(|i| i.sector))
                    .collect(),
            ))
            .collect();
        assert_eq!(summary, [("a.bin", 3000, vec![0]), ("b.bin", 4500, vec![150])]);
    }

    #[test]
    fn audio_tracks_without_pregaps_stay_separate() {
        const SHEET: &str = "\
FILE \"game.bin\" BINARY
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
";
        let (_, generated) =
            from_layout(SHEET, &[30_000], &[(30_000, SectorType::Audio)]);
        assert_same_sheet(&generated, SHEET);
    }

    /// Data, then a data track with a two-second pregap.
    const DATA_PREGAP: &str = "\
FILE \"game.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
  TRACK 02 MODE2/2352
    INDEX 00 01:00:00
    INDEX 01 01:02:00
";

    #[test]
    fn a_data_pregap_split_by_analysis_is_still_one_pregap() {
        // The analyzer types each pregap sector on its own, so one damaged
        // sector splits the pregap into runs.
        let (_, generated) = from_layout(DATA_PREGAP, &[5650], &[
            (4500, SectorType::Mode1),
            (100, SectorType::PregapMode2),
            (1, SectorType::PregapMode2Exception),
            (49, SectorType::PregapMode2),
            (1000, SectorType::Mode2Form1),
        ]);
        assert_same_sheet(&generated, DATA_PREGAP);
    }

    #[test]
    fn a_blank_data_pregap_keeps_its_index_00() {
        // An all-zero pregap sector is analyzed as zeroed data, not pregap.
        let (_, generated) = from_layout(DATA_PREGAP, &[5650], &[
            (4500, SectorType::Mode1),
            (150, SectorType::ZeroedMode2Data),
            (1000, SectorType::Mode2Form1),
        ]);
        assert_same_sheet(&generated, DATA_PREGAP);
    }

    #[test]
    fn the_layout_needs_one_length_per_file() {
        let sheet = parse_cue("game.cue", DATA_PREGAP).unwrap();
        assert!(matches!(
            disc_files(&sheet, &[1, 2]),
            Err(MapperError::FileCountMismatch(2, 1))
        ));
    }

    // Archives from before format 2.1 have no layout, so these pin what
    // the inferred CUE sheet looks like.

    #[test]
    fn without_a_layout_a_single_data_track_is_inferred() {
        assert_eq!(
            inferred(&[(1000, SectorType::Mode2Form1)]),
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
        );
    }

    #[test]
    fn without_a_layout_an_audio_pregap_starts_a_track() {
        assert_eq!(
            inferred(&[
                (3000, SectorType::Mode1),
                (150, SectorType::PregapAudio),
                (4500, SectorType::Audio),
            ]),
            "FILE \"game.bin\" BINARY\n\
            \x20 TRACK 01 MODE1/2352\n\
            \x20   INDEX 01 00:00:00\n\
            \x20 TRACK 02 AUDIO\n\
            \x20   INDEX 00 00:40:00\n\
            \x20   INDEX 01 00:42:00\n"
        );
    }

    #[test]
    fn without_a_layout_sector_types_of_one_mode_stay_in_one_track() {
        assert_eq!(
            inferred(&[
                (100, SectorType::Mode2Form1),
                (5, SectorType::Mode2Form1Exception),
                (20, SectorType::Mode2Form2),
                (50, SectorType::ZeroedMode2Data),
            ]),
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"
        );
    }
}
