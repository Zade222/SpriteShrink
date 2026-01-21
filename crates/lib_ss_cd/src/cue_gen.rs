use std::fmt::Write;

use sprite_shrink::Hashable;

use crate::lib_structs::{DiscManifest, MsfTime, SectorType, TrackMode};

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
