use crate::lib_structs::{
    CueSheet, CueFile, DiscArea, Track, TrackType, TrackIndex, MsfTime
};

use std::{collections::BTreeSet, sync::LazyLock};
use regex::Regex;
use thiserror::Error;
use tracing::warn;


#[derive(Error, Debug, PartialEq, Eq)]
pub enum ParseError {
    #[error("Invalid or unexpected line format: {0}")]
    InvalidLine(String),
    #[error("Unexpected command '{0}' found outside of a FILE or TRACK context")]
    UnexpectedCommand(String),
    #[error("A TRACK or INDEX command was found before a FILE command")]
    MissingFile,
    #[error("An INDEX command was found before a TRACK command")]
    MissingTrack,
    #[error("Invalid MM:SS:FF timestamp format: {0}:{1}:{2}")]
    InvalidTimestamp(String, String, String),
    #[error("Failed to parse number from string: {0}")]
    ParseIntError(String),
    #[error("Invalid track type: {0}")]
    InvalidTrackType(String),
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    #[error("The CUE sheet has no TRACK in a FILE")]
    NoTracks,
    #[error("{command} in track {track} is out of place: PREGAP must come \
        before the track's first INDEX and POSTGAP after its last, each at \
        most once")]
    MisplacedGap { command: &'static str, track: u8 },
    #[error("The GD-ROM area markers are out of order: {0}")]
    InvalidArea(&'static str),
    #[error("Multi-session discs aren't supported yet: {0}")]
    MultiSession(String),
}

/*Keywords are matched case-insensitively, as most CUE readers do, and only
at the start of a line, so that a REM or TITLE line that mentions INDEX
isn't read as one.

(?i-u:...) folds case for ASCII only, and only for the keyword. Unicode case
folding needs regex's unicode-case feature, which this crate doesn't enable;
without it a (?i) pattern fails to build at run time. Track and file types
are compared in upper case after matching.

The patterns are constants, so whether they build doesn't depend on the
input. The parser tests build every one, but cargo can enable extra regex
features for test builds, so the CLI tests are what check the release
feature set.*/
static RE_FILE: LazyLock<Regex> = LazyLock::new(
    || Regex::new(r#"^(?i-u:FILE)\s+"([^"]+)"\s+(\w+)"#)
        .expect("the FILE pattern is valid")
);
static RE_TRACK: LazyLock<Regex> = LazyLock::new(
    || Regex::new(r"^(?i-u:TRACK)\s+(\d+)\s+([\w/]+)")
        .expect("the TRACK pattern is valid")
);
static RE_INDEX: LazyLock<Regex> = LazyLock::new(
    || Regex::new(r"^(?i-u:INDEX)\s+(\d+)\s+(\d{2}):(\d{2}):(\d{2})")
        .expect("the INDEX pattern is valid")
);
static RE_GAP: LazyLock<Regex> = LazyLock::new(
    || Regex::new(r"^(?i-u:(PREGAP|POSTGAP))\s+(\d{2}):(\d{2}):(\d{2})")
        .expect("the PREGAP/POSTGAP pattern is valid")
);


fn parse_track_type(
    track: &str,
) -> Result<TrackType, ParseError> {
    match track.to_uppercase().as_str() {
        "AUDIO" => {
            Ok(TrackType::Audio)
        }
        "MODE1/2352" => {
            Ok(TrackType::Mode1_2352)
        }
        "MODE2/2352" => {
            Ok(TrackType::Mode2_2352)
        }
        _ => {
            Err(ParseError::InvalidTrackType(track.to_string()))
        }
    }
}


/// Parses a CUE sheet.
///
/// FILE, TRACK, INDEX, PREGAP and POSTGAP are kept, along with the GD-ROM
/// area markers `REM SINGLE-DENSITY AREA` and `REM HIGH-DENSITY AREA`.
/// Other commands (FLAGS, CATALOG, ISRC, CD-Text, other REM lines) don't
/// change where the disc's data is, so they're dropped, with one warning
/// per sheet naming them.
///
/// # Errors
///
/// Returns a `ParseError` if the sheet has no tracks, its commands are out
/// of order, a value is out of range, it uses a file or track type this
/// crate can't store, or it describes a multi-session disc.
pub fn parse_cue(
    source_filename: &str,
    content: &str
) -> Result<CueSheet, ParseError> {
    let (sheet, dropped) = parse_cue_lines(source_filename, content)?;
    if !dropped.is_empty() {
        let dropped: Vec<&str> = dropped.iter().map(String::as_str).collect();
        warn!(
            "{source_filename}: these CUE commands aren't stored, so the \
            extracted CUE sheet won't have them: {}",
            dropped.join(", ")
        );
    }
    Ok(sheet)
}

/// Where a sheet is in its GD-ROM area markers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AreaMarkers {
    /// No marker so far: a CD, or a GD-ROM sheet before its first line.
    None,
    SingleDensity,
    HighDensity,
}

/// `parse_cue`, also returning the first word of each line it didn't use,
/// in upper case.
fn parse_cue_lines(
    source_filename: &str,
    content: &str
) -> Result<(CueSheet, BTreeSet<String>), ParseError> {
    let mut cue_sheet = CueSheet {
        source_filename: source_filename.chars().take(255).collect(),
        files: Vec::new()
    };
    let mut current_file: Option<CueFile> = None;
    let mut current_track: Option<Track> = None;
    let mut seen_track = false;
    let mut markers = AreaMarkers::None;
    let mut dropped = BTreeSet::new();

    /*Some editors save CUE files with a UTF-8 byte order mark, which would
    otherwise stop the first line from matching.*/
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = RE_INDEX.captures(trimmed) {
            let track = current_track
                .as_mut()
                .ok_or(ParseError::MissingTrack)?;
            read_index(track, &caps)?;
        } else if let Some(caps) = RE_GAP.captures(trimmed) {
            let track = current_track
                .as_mut()
                .ok_or(ParseError::MissingTrack)?;
            read_gap(track, &caps)?;
        } else if let Some(caps) = RE_TRACK.captures(trimmed) &&
            let (Some(num_match), Some(type_match)) = (caps.get(1), caps.get(2))
        {
            finish_track(&mut current_file, current_track.take())?;
            if current_file.is_none() {
                return Err(ParseError::MissingFile);
            }

            let num_str = num_match.as_str();
            let number: u8 = num_str
                .parse()
                .map_err(|_| ParseError::ParseIntError(num_str.to_string()))?;

            let type_str = type_match.as_str();
            let track_type = parse_track_type(type_str)?;

            seen_track = true;
            current_track = Some(Track {
                number,
                track_type,
                indices: Vec::new(),
                pregap: 0,
                postgap: 0,
                area: if markers == AreaMarkers::HighDensity {
                    DiscArea::HighDensity
                } else {
                    DiscArea::SingleDensity
                },
            });
        } else if let Some(caps) = RE_FILE.captures(trimmed) &&
            let (Some(name_match), Some(type_match)) = (caps.get(1), caps.get(2))
        {
            finish_track(&mut current_file, current_track.take())?;
            if let Some(file) = current_file.take() {
                cue_sheet.files.push(file);
            }

            let file_type = type_match.as_str();
            if file_type.to_uppercase() != "BINARY" {
                return Err(
                    ParseError::UnsupportedFileType(file_type.to_string())
                );
            }

            current_file = Some(CueFile {
                name: name_match.as_str().to_string(),
                tracks: Vec::new()
            });
        } else if !read_rem(trimmed, seen_track, &mut markers)?
            && let Some(word) = trimmed.split_whitespace().next()
        {
            dropped.insert(word.to_ascii_uppercase());
        }
    }

    if markers == AreaMarkers::SingleDensity {
        return Err(ParseError::InvalidArea(
            "REM SINGLE-DENSITY AREA without a REM HIGH-DENSITY AREA"
        ));
    }

    finish_track(&mut current_file, current_track.take())?;

    if let Some(file) = current_file.take() {
        cue_sheet.files.push(file);
    }

    /*Lines the parser doesn't recognise are skipped, so a file that isn't
    a CUE sheet at all would otherwise parse as an empty disc.*/
    if cue_sheet.files.iter().all(|file| file.tracks.is_empty()) {
        return Err(ParseError::NoTracks);
    }

    Ok((cue_sheet, dropped))
}

/// Adds `track`, if there is one, to the file being read.
fn finish_track(
    file: &mut Option<CueFile>,
    track: Option<Track>,
) -> Result<(), ParseError> {
    if let Some(track) = track {
        file.as_mut().ok_or(ParseError::MissingFile)?.tracks.push(track);
    }
    Ok(())
}

/// Adds the INDEX that `caps` matched (`RE_INDEX`) to `track`. An INDEX
/// can't follow the track's POSTGAP.
fn read_index(
    track: &mut Track,
    caps: &regex::Captures<'_>,
) -> Result<(), ParseError> {
    if track.postgap > 0 {
        return Err(ParseError::MisplacedGap {
            command: "POSTGAP",
            track: track.number,
        });
    }
    if let (Some(num), Some(min), Some(sec), Some(fra)) =
        (caps.get(1), caps.get(2), caps.get(3), caps.get(4))
    {
        let number: u8 = num
            .as_str()
            .parse()
            .map_err(|_| ParseError::ParseIntError(num.as_str().to_string()))?;
        let position = parse_msf(min.as_str(), sec.as_str(), fra.as_str())?;
        track.indices.push(TrackIndex { number, position });
    }
    Ok(())
}

/// Sets `track`'s PREGAP or POSTGAP from what `caps` matched (`RE_GAP`).
/// PREGAP must come before the track's first INDEX and POSTGAP after one,
/// each at most once.
fn read_gap(
    track: &mut Track,
    caps: &regex::Captures<'_>,
) -> Result<(), ParseError> {
    let (Some(keyword), Some(min), Some(sec), Some(fra)) =
        (caps.get(1), caps.get(2), caps.get(3), caps.get(4))
    else {
        return Ok(());
    };
    let gap = parse_msf(min.as_str(), sec.as_str(), fra.as_str())?
        .to_total_frames();
    let is_pregap = keyword.as_str().eq_ignore_ascii_case("PREGAP");

    let (slot, in_place) = if is_pregap {
        (&mut track.pregap, track.indices.is_empty())
    } else {
        (&mut track.postgap, !track.indices.is_empty())
    };
    if !in_place || *slot > 0 {
        return Err(ParseError::MisplacedGap {
            command: if is_pregap { "PREGAP" } else { "POSTGAP" },
            track: track.number,
        });
    }
    *slot = gap;
    Ok(())
}

/// Reads a REM line the parser acts on: a GD-ROM area marker, or a
/// session number. Returns `false` for any other line, which the caller
/// drops.
///
/// A GD-ROM sheet marks the single-density area before its first track
/// and the high-density area once, after at least one track. `REM SESSION`
/// above 1 means a multi-session disc, which isn't supported yet.
fn read_rem(
    line: &str,
    seen_track: bool,
    markers: &mut AreaMarkers,
) -> Result<bool, ParseError> {
    let words: Vec<String> = line.split_whitespace()
        .map(str::to_ascii_uppercase)
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();

    match words.as_slice() {
        ["REM", "SINGLE-DENSITY", "AREA"] => {
            if *markers != AreaMarkers::None || seen_track {
                return Err(ParseError::InvalidArea(
                    "REM SINGLE-DENSITY AREA must come once, before the \
                    first track"
                ));
            }
            *markers = AreaMarkers::SingleDensity;
            Ok(true)
        }
        ["REM", "HIGH-DENSITY", "AREA"] => {
            match *markers {
                AreaMarkers::None => Err(ParseError::InvalidArea(
                    "REM HIGH-DENSITY AREA without a REM SINGLE-DENSITY AREA \
                    before the first track"
                )),
                AreaMarkers::HighDensity => Err(ParseError::InvalidArea(
                    "REM HIGH-DENSITY AREA appears more than once"
                )),
                AreaMarkers::SingleDensity if !seen_track => {
                    Err(ParseError::InvalidArea(
                        "the single-density area has no tracks"
                    ))
                }
                AreaMarkers::SingleDensity => {
                    *markers = AreaMarkers::HighDensity;
                    Ok(true)
                }
            }
        }
        ["REM", "SESSION", number, ..] => match number.parse::<u32>() {
            Ok(0 | 1) => Ok(true),
            Ok(_) => Err(ParseError::MultiSession(line.to_string())),
            Err(_) => Ok(false),
        },
        _ => Ok(false),
    }
}


fn parse_msf(
    minute_str: &str,
    second_str: &str,
    frame_str: &str
) -> Result<MsfTime, ParseError> {
    let minute: u8 = minute_str
        .parse()
        .map_err(|_| ParseError::ParseIntError(minute_str.to_string()))?;
    let second: u8 = second_str
        .parse()
        .map_err(|_| ParseError::ParseIntError(second_str.to_string()))?;
    let frame: u8 = frame_str
        .parse()
        .map_err(|_| ParseError::ParseIntError(frame_str.to_string()))?;

    if second >= 60 || frame >= 75 {
        return Err(ParseError::InvalidTimestamp(
            minute_str.to_string(),
            second_str.to_string(),
            frame_str.to_string()
        ));
    }

    Ok(MsfTime { minute, second, frame })
}


#[cfg(test)]
mod tests {
    use super::*;

    fn msf(minute: u8, second: u8, frame: u8) -> MsfTime {
        MsfTime { minute, second, frame }
    }

    fn index(number: u8, position: MsfTime) -> TrackIndex {
        TrackIndex { number, position }
    }

    /// A single-density track with no PREGAP or POSTGAP.
    fn track(number: u8, track_type: TrackType, indices: Vec<TrackIndex>)
        -> Track
    {
        Track {
            number,
            track_type,
            indices,
            pregap: 0,
            postgap: 0,
            area: DiscArea::SingleDensity,
        }
    }

    fn parse(content: &str) -> Result<CueSheet, ParseError> {
        parse_cue("game.cue", content)
    }

    /// A single-bin PS1 disc: one data track, then two audio tracks with
    /// pregaps.
    const PS1: &str = "\
FILE \"game.bin\" BINARY
  TRACK 01 MODE2/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 41:10:20
    INDEX 01 41:12:20
  TRACK 03 AUDIO
    INDEX 00 44:01:70
    INDEX 01 44:03:70
";

    fn ps1_files() -> Vec<CueFile> {
        vec![CueFile {
            name: "game.bin".to_string(),
            tracks: vec![
                track(1, TrackType::Mode2_2352, vec![index(1, msf(0, 0, 0))]),
                track(2, TrackType::Audio, vec![
                    index(0, msf(41, 10, 20)),
                    index(1, msf(41, 12, 20)),
                ]),
                track(3, TrackType::Audio, vec![
                    index(0, msf(44, 1, 70)),
                    index(1, msf(44, 3, 70)),
                ]),
            ],
        }]
    }

    #[test]
    fn a_single_bin_disc_parses() {
        let sheet = parse(PS1).unwrap();
        assert_eq!(sheet.source_filename, "game.cue");
        assert_eq!(sheet.files, ps1_files());
    }

    #[test]
    fn a_multi_bin_disc_keeps_each_file_s_tracks() {
        let sheet = parse("\
FILE \"game (Track 1).bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE \"game (Track 2).bin\" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
").unwrap();

        assert_eq!(sheet.files, vec![
            CueFile {
                name: "game (Track 1).bin".to_string(),
                tracks: vec![
                    track(1, TrackType::Mode1_2352, vec![index(1, msf(0, 0, 0))]),
                ],
            },
            CueFile {
                name: "game (Track 2).bin".to_string(),
                tracks: vec![track(2, TrackType::Audio, vec![
                    index(0, msf(0, 0, 0)),
                    index(1, msf(0, 2, 0)),
                ])],
            },
        ]);
    }

    #[test]
    fn gaps_are_kept_and_other_commands_are_dropped() {
        let (sheet, dropped) = parse_cue_lines("game.cue", "\
CATALOG 0000000000000
REM GENRE Game
REM INDEX 01 99:99:99
FILE \"game.bin\" BINARY
  TRACK 01 MODE2/2352
    FLAGS DCP
    TITLE \"INDEX 05 01:00:00\"
    INDEX 01 00:00:00
    POSTGAP 00:01:00
  TRACK 02 AUDIO
    INDEX 00 41:10:20
    INDEX 01 41:12:20
  TRACK 03 AUDIO
    pregap 00:02:00
    INDEX 00 44:01:70
    INDEX 01 44:03:70
").unwrap();

        let mut expected = ps1_files();
        expected[0].tracks[0].postgap = 75;
        expected[0].tracks[2].pregap = 150;
        assert_eq!(sheet.files, expected);
        assert_eq!(
            dropped,
            BTreeSet::from(["CATALOG", "FLAGS", "REM", "TITLE"].map(String::from))
        );
    }

    #[test]
    fn gaps_out_of_place_are_errors() {
        let gap_error = |command, track| ParseError::MisplacedGap {
            command,
            track,
        };
        for (body, expected) in [
            // PREGAP after an INDEX, and twice.
            ("INDEX 01 00:00:00\nPREGAP 00:02:00\n", gap_error("PREGAP", 1)),
            ("PREGAP 00:02:00\nPREGAP 00:02:00\n", gap_error("PREGAP", 1)),
            // POSTGAP before any INDEX, twice, and followed by an INDEX.
            ("POSTGAP 00:02:00\n", gap_error("POSTGAP", 1)),
            (
                "INDEX 01 00:00:00\nPOSTGAP 00:01:00\nPOSTGAP 00:01:00\n",
                gap_error("POSTGAP", 1),
            ),
            (
                "INDEX 00 00:00:00\nPOSTGAP 00:01:00\nINDEX 01 00:02:00\n",
                gap_error("POSTGAP", 1),
            ),
        ] {
            let content =
                format!("FILE \"a.bin\" BINARY\nTRACK 01 AUDIO\n{body}");
            assert_eq!(parse(&content), Err(expected), "{body:?}");
        }

        assert_eq!(
            parse("FILE \"a.bin\" BINARY\nPREGAP 00:02:00\n"),
            Err(ParseError::MissingTrack)
        );
    }

    /// A Dreamcast disc as Redump stores it: one BIN per track, with the
    /// high-density area starting at track 3.
    const GDROM: &str = "\
REM SINGLE-DENSITY AREA
FILE \"t1.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE \"t2.bin\" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
REM HIGH-DENSITY AREA
FILE \"t3.bin\" BINARY
  TRACK 03 MODE1/2352
    INDEX 01 00:00:00
FILE \"t4.bin\" BINARY
  TRACK 04 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00
";

    #[test]
    fn gdrom_area_markers_set_each_track_s_area() {
        let (sheet, dropped) = parse_cue_lines("game.cue", GDROM).unwrap();
        let areas: Vec<DiscArea> = sheet.files.iter()
            .flat_map(|file| &file.tracks)
            .map(|track| track.area)
            .collect();
        assert_eq!(areas, [
            DiscArea::SingleDensity,
            DiscArea::SingleDensity,
            DiscArea::HighDensity,
            DiscArea::HighDensity,
        ]);
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn gdrom_area_markers_out_of_order_are_errors() {
        const SINGLE: &str = "REM SINGLE-DENSITY AREA\n";
        const HIGH: &str = "REM HIGH-DENSITY AREA\n";
        const TRACK: &str = "FILE \"a.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n";

        for (name, content) in [
            ("high without single", format!("{TRACK}{HIGH}{TRACK}")),
            ("single after a track", format!("{TRACK}{SINGLE}{HIGH}{TRACK}")),
            ("single twice", format!("{SINGLE}{SINGLE}{TRACK}{HIGH}{TRACK}")),
            ("high twice", format!("{SINGLE}{TRACK}{HIGH}{TRACK}{HIGH}{TRACK}")),
            ("single with no tracks", format!("{SINGLE}{HIGH}{TRACK}")),
            ("no high-density area", format!("{SINGLE}{TRACK}")),
        ] {
            assert!(
                matches!(parse(&content), Err(ParseError::InvalidArea(_))),
                "{name}: {:?}",
                parse(&content)
            );
        }
    }

    #[test]
    fn only_a_second_session_makes_a_sheet_unsupported() {
        let two_sessions = format!(
            "REM SESSION 01\n{PS1}REM LEAD-OUT 00:30:00\nREM SESSION 02\n\
             FILE \"b.bin\" BINARY\nTRACK 04 MODE2/2352\nINDEX 01 00:00:00\n"
        );
        assert_eq!(
            parse(&two_sessions),
            Err(ParseError::MultiSession("REM SESSION 02".to_string()))
        );

        // One session, with the lead-out some rippers record: the REM lines
        // are dropped and the disc parses.
        let one_session = format!("REM SESSION 01\n{PS1}REM LEAD-OUT 00:30:00\n");
        let (sheet, dropped) = parse_cue_lines("game.cue", &one_session).unwrap();
        assert_eq!(sheet.files, ps1_files());
        assert_eq!(dropped, BTreeSet::from(["REM".to_string()]));
    }

    #[test]
    fn lowercase_keywords_crlf_and_a_bom_are_accepted() {
        let lowercase = PS1
            .replace("FILE", "file")
            .replace("TRACK", "Track")
            .replace("INDEX", "index")
            .replace("BINARY", "binary")
            .replace("MODE2", "mode2")
            .replace("AUDIO", "audio");
        let crlf = PS1.replace('\n', "\r\n");
        let bom = format!("\u{feff}{PS1}");
        let all = format!("\u{feff}{}", lowercase.replace('\n', "\r\n"));

        for (name, content) in [
            ("lowercase", &lowercase),
            ("crlf", &crlf),
            ("bom", &bom),
            ("all", &all),
        ] {
            assert_eq!(parse(content).unwrap().files, ps1_files(), "{name}");
        }
    }

    #[test]
    fn a_sheet_without_tracks_is_an_error() {
        for content in [
            "",
            "REM nothing here\n",
            "FILE \"game.bin\" BINARY\n",
            // An unquoted file name isn't matched, so the TRACK has no
            // FILE.
            "FILE game.bin BINARY\n",
        ] {
            assert_eq!(parse(content), Err(ParseError::NoTracks), "{content:?}");
        }
    }

    #[test]
    fn commands_out_of_order_are_errors() {
        assert_eq!(
            parse("TRACK 01 MODE1/2352\n  INDEX 01 00:00:00\n"),
            Err(ParseError::MissingFile)
        );
        assert_eq!(
            parse("FILE \"a.bin\" BINARY\n  INDEX 01 00:00:00\n"),
            Err(ParseError::MissingTrack)
        );
    }

    #[test]
    fn unsupported_track_and_file_types_are_errors() {
        assert_eq!(
            parse("FILE \"a.iso\" BINARY\n  TRACK 01 MODE1/2048\n"),
            Err(ParseError::InvalidTrackType("MODE1/2048".to_string()))
        );
        assert_eq!(
            parse("FILE \"a.wav\" WAVE\n  TRACK 01 AUDIO\n"),
            Err(ParseError::UnsupportedFileType("WAVE".to_string()))
        );
    }

    #[test]
    fn timestamps_out_of_range_are_errors() {
        for (time, parts) in [
            ("00:60:00", ("00", "60", "00")),
            ("00:00:75", ("00", "00", "75")),
        ] {
            let content =
                format!("FILE \"a.bin\" BINARY\n TRACK 01 AUDIO\n INDEX 01 {time}\n");
            assert_eq!(
                parse(&content),
                Err(ParseError::InvalidTimestamp(
                    parts.0.to_string(),
                    parts.1.to_string(),
                    parts.2.to_string()
                )),
                "{time}"
            );
        }
    }

    #[test]
    fn the_largest_valid_timestamp_parses() {
        let sheet = parse(
            "FILE \"a.bin\" BINARY\n TRACK 01 AUDIO\n INDEX 01 99:59:74\n"
        ).unwrap();
        assert_eq!(
            sheet.files[0].tracks[0].indices,
            vec![index(1, msf(99, 59, 74))]
        );
    }

    #[test]
    fn the_source_file_name_is_cut_to_255_characters() {
        // Multi-byte characters, so a cut by bytes would differ.
        let long_name = "é".repeat(300);
        let sheet = parse_cue(&long_name, PS1).unwrap();
        assert_eq!(sheet.source_filename, "é".repeat(255));
    }

    #[test]
    fn a_parsed_sheet_prints_back_to_the_same_sheet() {
        let with_gaps = PS1
            .replace("  TRACK 02 AUDIO\n", "  TRACK 02 AUDIO\n    PREGAP 00:02:00\n")
            + "    POSTGAP 00:02:00\n";
        for content in [PS1, &with_gaps, GDROM] {
            let sheet = parse(content).unwrap();
            assert_eq!(parse(&sheet.to_cue_string()).unwrap(), sheet, "{content}");
        }
    }
}
