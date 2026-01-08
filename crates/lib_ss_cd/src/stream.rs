use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use thiserror::Error;


use crate::lib_structs::{
    SectorMap, SectorType
};

#[derive(Error, Debug)]
pub enum StreamError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A stream that provides a unified `Read` and `Seek` interface over multiple
/// underlying `.bin` files.
///
/// This struct is designed to handle disc images that are split across several
/// individual `.bin` files (e.g., in a multi-track CUE/BIN setup). It presents
/// these multiple files as a single, contiguous virtual stream, abstracting
/// away the complexity of managing file boundaries and seeking between them.
///
/// I/O operations (read, seek) are automatically delegated to the correct
/// underlying `File` based on the current virtual position.
pub struct MultiBinStream {
    files: Vec<File>,
    file_boundaries: Vec<u64>,
    virtual_pos: u64,
    total_len: u64,
}

impl MultiBinStream {
    /// Creates a new `MultiBinStream` from a list of paths to individual BIN
    /// files.
    ///
    /// The provided paths are expected to be ordered correctly, representing
    /// the sequential layout of the virtual disc image. Each file is opened,
    /// and its length is used to calculate the virtual boundaries within the
    /// combined stream.
    ///
    /// # Arguments
    ///
    /// * `paths`: A `Vec<PathBuf>` containing the paths to the `.bin` files,
    ///   ordered from the first part of the disc to the last.
    ///
    /// # Returns
    ///
    /// A `Result` which is:
    /// - `Ok(Self)`: A new `MultiBinStream` instance ready for reading and
    ///   seeking.
    /// - `Err(io::Error)`: If any of the specified files cannot be opened or
    ///   their metadata (specifically, their length) cannot be retrieved.
    pub fn new(paths: Vec<PathBuf>) -> io::Result<Self> {
        let mut files = Vec::with_capacity(paths.len());
        let mut file_boundaries = Vec::with_capacity(paths.len());
        let mut total_len: u64 = 0;

        for path in paths {
            let file = File::open(path)?;
            let file_len = file.metadata()?.len();
            total_len += file_len;
            files.push(file);
            file_boundaries.push(total_len);
        }

        Ok(Self {
            files,
            file_boundaries,
            virtual_pos: 0,
            total_len,
        })
    }
}


impl Seek for MultiBinStream {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(p) => self.total_len as i64 + p,
            SeekFrom::Current(p) => self.virtual_pos as i64 + p,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            ));
        }

        self.virtual_pos = new_pos as u64;
        Ok(self.virtual_pos)
    }
}

impl Read for MultiBinStream {
    /// Seeks to a new virtual position within the aggregated stream of BIN
    /// files.
    ///
    /// This method calculates and updates the internal `virtual_pos` based on
    /// the `SeekFrom` argument. The actual seeking operation on the underlying
    /// `File`s is deferred until a `read` operation is performed, which then
    /// determines the correct file and relative offset to seek to.
    ///
    /// # Arguments
    ///
    /// * `pos`: A `SeekFrom` variant specifying the origin and offset for the
    ///   seek operation (e.g., `SeekFrom::Start`, `SeekFrom::End`,
    ///   `SeekFrom::Current`).
    ///
    /// # Returns
    ///
    /// A `Result` which is:
    /// - `Ok(u64)`: The new virtual cursor position (offset from the beginning
    ///   of the aggregated stream).
    /// - `Err(io::Error)`: If an attempt is made to seek to a negative or
    ///   overflowing position, or if an underlying I/O error occurs (though
    ///   actual file errors are typically caught during `read`).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.virtual_pos >= self.total_len {
            return Ok(0);
        }

        let file_idx = self
            .file_boundaries
            .iter()
            .position(|&boundary| self.virtual_pos < boundary)
            .unwrap_or(self.files.len() - 1);

        let base_offset = if file_idx > 0 {
            self.file_boundaries[file_idx - 1]
        } else {
            0
        };

        let relative_offset = self.virtual_pos - base_offset;
        let file = &mut self.files[file_idx];
        file.seek(SeekFrom::Start(relative_offset))?;

        let bytes_read = file.read(buf)?;
        self.virtual_pos += bytes_read as u64;

        Ok(bytes_read)
    }
}
/*
/// A stream adapter that provides `Read` and `Seek` access to a specific range
/// of sectors within an underlying source.
///
/// This struct allows treating a contiguous block of sectors from a larger
/// `Read + Seek` source (like a `File` or `MultiBinStream`) as if it were
/// a standalone stream. It operates on 2352-byte sectors, which is the standard
/// size for CD-ROM raw sectors.
///
/// It manages an internal buffer to read sectors one by one from the source
/// and presents their data to the caller. Seeking and reading outside the
/// defined sector range will result in an error or an early end-of-file.
pub struct SectorRegionStream<'a, R: Read + Seek> {
    pub source: &'a mut R,
    pub current_sector_idx: u32,
    pub end_sector_idx: u32,
    pub sector_buffer: Box<[u8; 2352]>,
    pub pos_in_sector: usize,
}*/

/// A stream adapter that provides `Read` and `Seek` access to a specific range
/// of sectors within an underlying source.
///
/// This struct allows treating a contiguous block of sectors from a larger
/// `Read + Seek` source (like a `File` or `MultiBinStream`) as if it were
/// a standalone stream. It operates on 2352-byte sectors, which is the standard
/// size for CD-ROM raw sectors.
///
/// It manages an internal buffer to read sectors one by one from the source
/// and presents their data to the caller. Seeking and reading outside the
/// defined sector range will result in an error or an early end-of-file.
pub struct SectorRegionStream<'a, R: Read + Seek> {
       source: &'a mut R,
       start_sector: u32,
       end_sector: u32,
       virtual_pos: u64,
       sector_buffer: Box<[u8; 2352]>,
       loaded_sector_idx: Option<u32>,
   }


impl<'a, R: Read + Seek> SectorRegionStream<'a, R> {
    /// Creates a new `SectorRegionStream` to read from a specific range of
    /// sectors.
    ///
    /// The stream will start reading from `start_absolute_sector` and continue
    /// for `sector_count` sectors.
    ///
    /// # Arguments
    ///
    /// * `source`: A mutable reference to the underlying `Read + Seek` source
    ///   (e.g., `File` or `MultiBinStream`).
    /// * `start_absolute_sector`: The 0-based absolute sector index in the
    ///   `source` where this region begins.
    /// * `sector_count`: The total number of sectors included in this region.
    ///   The stream will stop reading after this many sectors have been
    ///   processed.
    ///
    /// # Returns
    ///
    /// A new `SectorRegionStream` instance configured to read within the
    /// specified sector range.
    pub fn new(
        source: &'a mut R,
        start_absolute_sector: u32,
        sector_count: u32,
    ) -> Self {
        Self {
            source,
            start_sector: start_absolute_sector,
            end_sector: start_absolute_sector.saturating_add(sector_count),
            virtual_pos: 0,
            sector_buffer: Box::new([0u8; 2352]),
            loaded_sector_idx: None,
        }
    }
    /*
    /// Reads the next complete 2352-byte sector from the underlying source
    /// into the internal sector buffer.
    ///
    /// This is an internal helper method used by the `Read` implementation
    /// to manage sector-by-sector access. It performs the necessary seek
    /// and read operations on the `source` to load the next sector's data.
    ///
    /// After successfully reading a sector, it resets the internal buffer's
    /// read position (`pos_in_sector`) to 0 and increments the
    /// `current_sector_idx`.
    ///
    /// # Errors
    ///
    /// - `io::ErrorKind::UnexpectedEof`: If an attempt is made to read beyond
    ///   the `end_sector_idx` of this `SectorRegionStream`.
    /// - Other `io::Error`s: Propagated from the underlying `source.seek()`
    ///   or `source.read_exact()` operations.
    fn process_next_sector(&mut self) -> io::Result<()> {
        if self.current_sector_idx >= self.end_sector_idx {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }

        let seek_pos = self.current_sector_idx as u64 * 2352;
        self.source.seek(SeekFrom::Start(seek_pos))?;
        self.source.read_exact(self.sector_buffer.as_mut())?;

        self.pos_in_sector = 0;

        self.current_sector_idx += 1;
        Ok(())
    }*/
}

impl<'a, R: Read + Seek> Seek for SectorRegionStream<'a, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let region_size_sectors = self.end_sector
            .saturating_sub(self.start_sector);
        let region_size_bytes = region_size_sectors as u64 * 2352;

        let new_pos = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(p) => region_size_bytes as i64 + p,
            SeekFrom::Current(p) => self.virtual_pos as i64 + p,
        };

        // Ensure the new position is within the bounds of the region.
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            ));
        }

        self.virtual_pos = new_pos as u64;
        Ok(self.virtual_pos)
    }
}


impl<'a, R: Read + Seek> Read for SectorRegionStream<'a, R> {
    /// Reads bytes from the stream's current virtual position into the
    /// provided buffer.
    ///
    /// This method implements the `std::io::Read` trait. It uses the stream's
    /// virtual position (`self.virtual_pos`), which can be manipulated by
    /// `seek`, to determine where to read from within the defined sector
    /// region.
    ///
    /// Based on the virtual position, it calculates which underlying sector
    /// needs to be read. If that sector is not already loaded into the
    /// internal buffer, this method will perform a `seek` and `read_exact`
    /// on the main `source` to load it.
    ///
    /// The method correctly handles read requests that span across multiple
    /// sector boundaries, loading new sectors as needed to fill the buffer.
    ///
    /// # Arguments
    ///
    /// * `buf`: The mutable slice of bytes into which the data will be read.
    ///
    /// # Returns
    ///
    /// A `Result` which is:
    /// - `Ok(usize)`: The number of bytes successfully read into `buf`. A
    ///   return value of `0` indicates that the virtual cursor is at or beyond
    ///   the end of the defined region.
    /// - `Err(io::Error)`: If an I/O error occurs on the underlying `source`
    ///   during a seek or read operation.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let region_size_bytes = (self.end_sector - self.start_sector) as u64 * 2352;
        let mut bytes_written_to_buf = 0;

        while bytes_written_to_buf < buf.len() {
            if self.virtual_pos >= region_size_bytes {
                return Ok(0);
            }

            let sector_num_in_region = (self.virtual_pos / 2352) as u32;
            let absolute_sector_to_load = self.start_sector + sector_num_in_region;

            if self.loaded_sector_idx != Some(absolute_sector_to_load) {
                if absolute_sector_to_load >= self.end_sector {
                    return Err(io::Error::other(
                        "Internal logic error: attempted to read past region boundary"
                    ))
                }

                let seek_pos = absolute_sector_to_load as u64 * 2352;
                self.source.seek(SeekFrom::Start(seek_pos))?;
                self.source.read_exact(self.sector_buffer.as_mut())?;
                self.loaded_sector_idx = Some(absolute_sector_to_load);
            }

            let pos_in_sector = (self.virtual_pos % 2352) as usize;

            let bytes_in_buffer_available = 2352 - pos_in_sector;
            let rem_in_caller_buf = buf.len() - bytes_written_to_buf;
            let bytes_to_copy = std::cmp::min(
                bytes_in_buffer_available,
                rem_in_caller_buf
            );

            let source_slice = &self.sector_buffer[
                pos_in_sector..pos_in_sector + bytes_to_copy
            ];
            let dest_slice = &mut buf[
                bytes_written_to_buf..bytes_written_to_buf + bytes_to_copy
            ];
            dest_slice.copy_from_slice(source_slice);

            self.virtual_pos += bytes_to_copy as u64;
            bytes_written_to_buf += bytes_to_copy;
        }

        Ok(bytes_written_to_buf)
    }
}

/// A stream adapter that filters a `Read + Seek` source of raw CD sectors,
/// yielding only the user data portions.
///
/// This struct is designed to process disc images by treating them as a
/// stream of sectors. It uses a `SectorMap` to identify the type of each
/// sector (e.g., Mode 1, Mode 2, Audio).
///
/// When read from, `UserDataStream` automatically skips over non-data sectors
/// (like audio) and for data sectors, it filters out the headers, subheaders,
/// and error correction/detection codes, presenting only the contiguous user
/// data to the caller.
pub struct UserDataStream<'a, R: Read + Seek> {
    source: &'a mut R,
    sector_map: &'a SectorMap,
    cur_sec_idx: u32,
    sec_buffer: Box<[u8; 2352]>,
    pos_in_user_data: usize,
    len_of_user_data: usize,
}


impl<'a, R: Read + Seek> UserDataStream<'a, R> {
    /// Creates a new `UserDataStream`.
    ///
    /// The stream will read from the provided `source` and use the
    /// `sector_map` to determine how to process each sector.
    ///
    /// # Arguments
    ///
    /// * `source`: A mutable reference to the underlying `Read + Seek` source,
    ///   which is expected to contain raw 2352-byte sectors.
    /// * `sector_map`: A reference to a `SectorMap` that provides the type
    ///   information for every sector in the `source`. This map guides the
    ///   stream on which sectors to read and which parts of them contain
    ///   user data.
    ///
    /// # Returns
    ///
    /// A new `UserDataStream` instance ready to be read from.
    pub fn new(
        source: &'a mut R,
        sector_map: &'a SectorMap,
    ) -> Self {
        Self {
            source,
            sector_map,
            cur_sec_idx: 0,
            sec_buffer: Box::new([0; 2352]),
            pos_in_user_data: 1,
            len_of_user_data: 0,
        }
    }

    /// Advances the stream to the next sector that contains user data, reads
    /// that sector into an internal buffer, and sets up pointers to expose
    /// only the user data portion.
    ///
    /// This internal helper method iterates through the sectors of the
    /// underlying `source` (as indicated by `self.cur_sec_idx`). It uses the
    /// `sector_map` to determine if a sector is a data sector
    /// (Mode 1, Mode 2 Form 1, Mode 2 Form 2).
    ///
    /// If a data sector is found:
    /// - It seeks to the sector's absolute position in the `source`.
    /// - Reads the full 2352-byte sector into `self.sec_buffer`.
    /// - Calculates the start and end byte offsets for the user data within
    ///   `self.sec_buffer` for that specific sector type.
    ///
    /// If the current sector is not a data sector (e.g., audio, pregap), it is
    /// skipped, and the method continues searching for the next data sector.
    ///
    /// # Errors
    ///
    /// - `io::ErrorKind::UnexpectedEof`: If the end of the `sector_map` (and
    ///   thus the end of the disc image as defined by the map) is reached
    ///   without finding any more user data sectors.
    /// - Other `io::Error`s: Propagated from the `source.seek()` or
    ///   `source.read_exact()` operations.
    fn process_next_sector(&mut self) -> io::Result<()> {
        loop {
            if self.cur_sec_idx as usize >= self.sector_map.sectors.len() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }

            let sector_type = &self.sector_map.sectors[self.cur_sec_idx as usize];

            let user_data_range = match sector_type {
                SectorType::Mode1 | SectorType::Mode1Exception =>
                    Some(16..2064),
                SectorType::Mode2Form1 | SectorType::Mode2Form1Exception =>
                    Some(24..2072),
                SectorType::Mode2Form2 | SectorType::Mode2Form2Exception =>
                    Some(24..2348),
                SectorType::PregapMode1 | SectorType::PregapMode1Exception =>
                    Some(16..2064),
                SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
                    Some(24..2072),
                SectorType::ZeroedData => Some(0..2352),
                // All other types (Audio, Pregap and PregapAudio) are skipped.
                _ => None,
            };

            if let Some(range) = user_data_range {
                let seek_pos = self.cur_sec_idx as u64 * 2352;
                self.source.seek(SeekFrom::Start(seek_pos))?;
                self.source.read_exact(self.sec_buffer.as_mut())?;

                self.pos_in_user_data = range.start;
                self.len_of_user_data = range.end;

                self.cur_sec_idx += 1;
                return Ok(());
            } else {
                // Not a data sector
                self.cur_sec_idx += 1;
                continue;
            }
        }
    }
}


impl<'a, R: Read + Seek> Read for UserDataStream<'a, R> {
    /// Reads user data bytes from the `UserDataStream` into the provided
    /// buffer.
    ///
    /// This method implements the `std::io::Read` trait, allowing the
    /// `UserDataStream` to behave as a standard byte stream that yields
    /// only user data. It fills `buf` by drawing data from the user data
    /// portion of its internal `sec_buffer`.
    ///
    /// When the user data portion of the current sector is exhausted, or if
    /// no user data sector has been loaded yet, it transparently calls
    /// `process_next_sector` to find and load the subsequent sector that
    /// contains user data from the underlying `source`.
    ///
    /// Reading will continue until `buf` is full, no more user data is
    /// available in the stream, or an I/O error occurs.
    ///
    /// # Arguments
    ///
    /// * `buf`: The mutable slice of bytes into which the user data will be
    ///   read.
    ///
    /// # Returns
    ///
    /// A `Result` which is:
    /// - `Ok(usize)`: The number of user data bytes successfully read into
    ///   `buf`.
    ///   A return value of `0` indicates that the end of the user data stream
    ///   has been reached.
    /// - `Err(io::Error)`: If an I/O error occurs during the read operation,
    ///   propagated from the underlying `source` or `process_next_sector`.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_written_to_buf = 0;

        while bytes_written_to_buf < buf.len() {
            if self.pos_in_user_data >= self.len_of_user_data {
                match self.process_next_sector() {
                    Ok(()) => continue,
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e),
                }
            }

            let rem_in_sector = self.len_of_user_data - self.pos_in_user_data;
            let rem_in_caller_buf = buf.len() - bytes_written_to_buf;
            let bytes_to_copy = std::cmp::min(rem_in_sector, rem_in_caller_buf);

            let source_slice = &self.sec_buffer[
                self.pos_in_user_data..self.pos_in_user_data + bytes_to_copy
            ];
            let dest_slice = &mut buf[
                bytes_written_to_buf..bytes_written_to_buf + bytes_to_copy
            ];
            dest_slice.copy_from_slice(source_slice);

            bytes_written_to_buf += bytes_to_copy;
            self.pos_in_user_data += bytes_to_copy;
        }

        Ok(bytes_written_to_buf)
    }
}
