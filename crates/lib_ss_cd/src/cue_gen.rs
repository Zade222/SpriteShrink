use std::fmt::Write;

use sprite_shrink::Hashable;

use crate::lib_structs::{DiscManifest, MsfTime, TrackMode};

pub fn generate_cue_string<H: Hashable>(
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
    use super::*;
    use crate::lib_structs::{DiscManifest, RleSectorMap, SectorType};

    fn manifest_with_runs(runs: Vec<(u32, SectorType)>) -> DiscManifest<u64> {
        DiscManifest {
            lba_map: Vec::new(),
            rle_sector_map: RleSectorMap { runs },
            audio_block_map: Vec::new(),
            data_stream_layout: Vec::new(),
            subheader_index: Vec::new(),
            disc_exception_index: Vec::new(),
            integrity_hash: 0,
        }
    }

    #[test]
    fn generate_cue_string_emits_track_for_each_mode_change() {
        let manifest = manifest_with_runs(vec![
            (150, SectorType::Mode1),
            (75, SectorType::Audio),
            (75, SectorType::Mode2Form1),
        ]);

        let cue = generate_cue_string(&manifest, "disc.bin").unwrap();

        assert!(cue.contains("FILE \"disc.bin\" BINARY"));
        assert!(cue.contains("  TRACK 01 MODE1/2352"));
        assert!(cue.contains("    INDEX 01 00:00:00"));
        assert!(cue.contains("  TRACK 02 AUDIO"));
        assert!(cue.contains("    INDEX 01 00:02:00"));
        assert!(cue.contains("  TRACK 03 MODE2/2352"));
        assert!(cue.contains("    INDEX 01 00:03:00"));
    }

    #[test]
    fn generate_cue_string_writes_index_zero_for_pregap() {
        let manifest = manifest_with_runs(vec![
            (75, SectorType::PregapAudio),
            (75, SectorType::Audio),
        ]);

        let cue = generate_cue_string(&manifest, "disc.bin").unwrap();

        assert!(cue.contains("  TRACK 01 AUDIO"));
        assert!(cue.contains("    INDEX 00 00:00:00"));
        assert!(cue.contains("    INDEX 01 00:01:00"));
    }

    #[test]
    fn generate_cue_string_skips_none_sectors_but_advances_time() {
        let manifest = manifest_with_runs(vec![
            (75, SectorType::None),
            (75, SectorType::Mode1),
        ]);

        let cue = generate_cue_string(&manifest, "disc.bin").unwrap();

        assert!(cue.contains("  TRACK 01 MODE1/2352"));
        assert!(cue.contains("    INDEX 01 00:01:00"));
    }
}
