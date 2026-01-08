use crate::lib_structs::{
    CueSheet, CueFile, Track, TrackType, TrackIndex, MsfTime
};

use once_cell::sync::Lazy;
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
}

static RE_FILE: Lazy<Regex> = Lazy::new(
    || Regex::new(r#"^FILE\s+"([^"]+)"\s+(\w+)"#).unwrap()
);
static RE_TRACK: Lazy<Regex> = Lazy::new(
    || Regex::new(r"^TRACK\s+(\d+)\s+([\w/]+)").unwrap()
);
static RE_INDEX: Lazy<Regex> = Lazy::new(
    || Regex::new(r"INDEX (\d+) (\d{2}):(\d{2}):(\d{2})").unwrap()
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
