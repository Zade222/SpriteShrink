use crate::lib_structs::{
    CueSheet, CueFile, Track, TrackType, TrackIndex, MsfTime
};

use std::sync::LazyLock;
use regex::Regex;
use thiserror::Error;


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


pub fn parse_cue(
    source_filename: &str,
    content: &str
) -> Result<CueSheet, ParseError> {
    let mut cue_sheet = CueSheet {
        source_filename: source_filename.chars().take(255).collect(),
        files: Vec::new()
    };
    let mut current_file: Option<CueFile> = None;
    let mut current_track: Option<Track> = None;

    /*Some editors save CUE files with a UTF-8 byte order mark, which would
    otherwise stop the first line from matching.*/
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(caps) = RE_INDEX.captures(trimmed) {
            let cur_track = current_track
                .as_mut()
                .ok_or(ParseError::MissingTrack)?;
            if let (Some(num), Some(min), Some(sec), Some(fra)) =
                (caps.get(1), caps.get(2), caps.get(3), caps.get(4))
            {
                let number: u8 = num
                    .as_str()
                    .parse()
                    .map_err(|_| ParseError::ParseIntError(
                        num.as_str().to_string()
                    ))?;

                let msf = parse_msf(min.as_str(), sec.as_str(), fra.as_str())?;

                cur_track.indices.push(TrackIndex { number, position: msf });

            }
        } else if let Some(caps) = RE_TRACK.captures(trimmed) &&
            let (Some(num_match), Some(type_match)) = (caps.get(1), caps.get(2))
        {
            if let Some(track) = current_track.take() {
                current_file
                    .as_mut()
                    .ok_or(ParseError::MissingFile)?.tracks
                    .push(track);
            }
            if current_file.is_none() {
                return Err(ParseError::MissingFile);
            }

            let num_str = num_match.as_str();
            let number: u8 = num_str
                .parse()
                .map_err(|_| ParseError::ParseIntError(num_str.to_string()))?;

            let type_str = type_match.as_str();
            let track_type = parse_track_type(type_str)?;

            current_track = Some(Track {
                number,
                track_type,
                indices: Vec::new()
            });
        } else if let Some(caps) = RE_FILE.captures(trimmed) &&
            let (Some(name_match), Some(type_match)) = (caps.get(1), caps.get(2))
        {
            if let Some(track) = current_track.take() {
                current_file
                    .as_mut()
                    .ok_or(ParseError::MissingFile)?.tracks
                    .push(track);
            }
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
        }
    }

    if let Some(track) = current_track.take() {
        current_file
            .as_mut()
            .ok_or(ParseError::MissingFile)?.tracks.push(track);
    }

    if let Some(file) = current_file.take() {
        cue_sheet.files.push(file);
    }

    /*Lines the parser doesn't recognise are skipped, so a file that isn't
    a CUE sheet at all would otherwise parse as an empty disc.*/
    if cue_sheet.files.iter().all(|file| file.tracks.is_empty()) {
        return Err(ParseError::NoTracks);
    }

    Ok(cue_sheet)
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

    fn track(number: u8, track_type: TrackType, indices: Vec<TrackIndex>)
        -> Track
    {
        Track { number, track_type, indices }
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
    fn other_commands_are_skipped() {
        let sheet = parse("\
CATALOG 0000000000000
REM GENRE Game
REM INDEX 01 99:99:99
FILE \"game.bin\" BINARY
  TRACK 01 MODE2/2352
    FLAGS DCP
    TITLE \"INDEX 05 01:00:00\"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 41:10:20
    INDEX 01 41:12:20
  TRACK 03 AUDIO
    PREGAP 00:02:00
    INDEX 00 44:01:70
    INDEX 01 44:03:70
").unwrap();
        assert_eq!(sheet.files, ps1_files());
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
        let sheet = parse(PS1).unwrap();
        assert_eq!(parse(&sheet.to_cue_string()).unwrap(), sheet);
    }
}
