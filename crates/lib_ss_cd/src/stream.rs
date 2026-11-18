use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

//use thiserror::Error;


use crate::lib_structs::{
    SectorMap, SectorType
};

/// The size of a raw CD sector in bytes.
const SECTOR_BYTES: u64 = 2352;

/*
#[derive(Error, Debug)]
pub enum StreamError {
    #[error(transparent)]
    Io(#[from] io::Error),
}*/

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


/// Works out the position a `seek` to `pos` lands on, for a stream that is
/// `len` bytes long and at `current`.
///
/// # Errors
///
/// `InvalidInput` if the position would be before the start of the stream
/// or past `u64::MAX`.
fn resolve_seek(pos: SeekFrom, current: u64, len: u64) -> io::Result<u64> {
    let new_pos = match pos {
        SeekFrom::Start(p) => Some(p),
        SeekFrom::End(delta) => len.checked_add_signed(delta),
        SeekFrom::Current(delta) => current.checked_add_signed(delta),
    };

    new_pos.ok_or_else(|| io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid seek to a negative or overflowing position",
    ))
}

impl Seek for MultiBinStream {
    /// Moves the stream's position. The files aren't touched until the next
    /// `read`, which seeks the file that holds the position.
    ///
    /// Seeking past the end is allowed, and a `read` there returns 0.
    ///
    /// # Errors
    ///
    /// `InvalidInput` if the position would be before the start or past
    /// `u64::MAX`. The position is unchanged.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.virtual_pos = resolve_seek(pos, self.virtual_pos, self.total_len)?;
        Ok(self.virtual_pos)
    }
}

impl Read for MultiBinStream {
    /// Reads from the file that holds the current position. A read stops at
    /// the end of that file, so it can return fewer bytes than `buf` holds
    /// before the end of the stream; the next read continues in the next
    /// file.
    ///
    /// # Errors
    ///
    /// Any error from seeking or reading the underlying file.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.virtual_pos >= self.total_len {
            return Ok(0);
        }

        // The first file that ends after the position holds it. Empty files
        // end where the one before them does, so they're never picked.
        let Some(file_idx) = self
            .file_boundaries
            .iter()
            .position(|&boundary| self.virtual_pos < boundary)
        else {
            // The last boundary is `total_len`, and the position is before
            // it, so some file holds it.
            return Err(io::Error::other("no file holds the stream position"));
        };

        let base_offset = file_idx
            .checked_sub(1)
            .and_then(|prev| self.file_boundaries.get(prev))
            .copied()
            .unwrap_or(0);

        let relative_offset = self.virtual_pos - base_offset;
        let Some(file) = self.files.get_mut(file_idx) else {
            // There's one boundary per file, so the index is in range.
            return Err(io::Error::other("no file holds the stream position"));
        };
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

impl<R: Read + Seek> SectorRegionStream<'_, R> {
    /// The length of the region in bytes.
    fn region_len(&self) -> u64 {
        u64::from(self.end_sector - self.start_sector) * SECTOR_BYTES
    }
}

impl<R: Read + Seek> Seek for SectorRegionStream<'_, R> {
    /// Moves the position within the region. Seeking past the end of the
    /// region is allowed, and a `read` there returns 0.
    ///
    /// # Errors
    ///
    /// `InvalidInput` if the position would be before the start of the
    /// region or past `u64::MAX`. The position is unchanged.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.virtual_pos = resolve_seek(pos, self.virtual_pos, self.region_len())?;
        Ok(self.virtual_pos)
    }
}


impl<R: Read + Seek> Read for SectorRegionStream<'_, R> {
    /// Reads bytes from the stream's current virtual position into the
    /// provided buffer.
    ///
    /// Based on the virtual position, it calculates which underlying sector
    /// needs to be read. If that sector is not already loaded into the
    /// internal buffer, this method will perform a `seek` and `read_exact`
    /// on the main `source` to load it. A read can span several sectors, and
    /// fills `buf` unless the region ends first.
    ///
    /// # Returns
    ///
    /// The number of bytes read into `buf`. `0` means the position is at or
    /// past the end of the region.
    ///
    /// # Errors
    ///
    /// Any error from seeking or reading `source`, including
    /// `UnexpectedEof` if the source ends before the region does.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let region_len = self.region_len();
        let mut bytes_written_to_buf = 0;

        // Stop at the end of the region, but still report the bytes this
        // call has already copied.
        while bytes_written_to_buf < buf.len() && self.virtual_pos < region_len {
            // The position is inside the region, so the sector number is
            // smaller than the region's sector count.
            let sector_num_in_region = u32::try_from(self.virtual_pos / SECTOR_BYTES)
                .map_err(io::Error::other)?;
            let absolute_sector_to_load = self.start_sector + sector_num_in_region;

            if self.loaded_sector_idx != Some(absolute_sector_to_load) {
                let seek_pos = u64::from(absolute_sector_to_load) * SECTOR_BYTES;
                self.source.seek(SeekFrom::Start(seek_pos))?;
                self.source.read_exact(self.sector_buffer.as_mut())?;
                self.loaded_sector_idx = Some(absolute_sector_to_load);
            }

            let pos_in_sector = usize::try_from(self.virtual_pos % SECTOR_BYTES)
                .map_err(io::Error::other)?;

            // Copy what's left of the sector, or what fits in `buf`.
            let source_slice = self.sector_buffer
                .get(pos_in_sector..)
                .unwrap_or_default();
            let dest_slice = buf
                .get_mut(bytes_written_to_buf..)
                .unwrap_or_default();
            let bytes_to_copy = source_slice.len().min(dest_slice.len());
            let (Some(src), Some(dst)) = (
                source_slice.get(..bytes_to_copy),
                dest_slice.get_mut(..bytes_to_copy),
            ) else {
                break;
            };
            dst.copy_from_slice(src);

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
    /// Sectors without user data in the stream (audio, `None`) are skipped.
    ///
    /// # Returns
    ///
    /// `true` if a sector was loaded, `false` if the sector map has no more
    /// data sectors.
    ///
    /// # Errors
    ///
    /// Any error from seeking or reading `source`, including
    /// `UnexpectedEof` if the source ends before the sector map does.
    fn process_next_sector(&mut self) -> io::Result<bool> {
        loop {
            let Some(sector_type) = usize::try_from(self.cur_sec_idx)
                .ok()
                .and_then(|idx| self.sector_map.sectors.get(idx))
            else {
                return Ok(false);
            };

            let user_data_range = match sector_type {
                SectorType::Mode1 | SectorType::Mode1Exception
                | SectorType::PregapMode1 | SectorType::PregapMode1Exception =>
                    Some(16..2064),
                SectorType::Mode2Form1 | SectorType::Mode2Form1Exception
                | SectorType::PregapMode2 | SectorType::PregapMode2Exception =>
                    Some(24..2072),
                SectorType::Mode2Form2 | SectorType::Mode2Form2Exception =>
                    Some(24..2348),
                SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data =>
                    Some(0..2352),
                SectorType::Audio | SectorType::PregapAudio
                | SectorType::ZeroedAudio | SectorType::None => None,
            };

            let sector_idx = self.cur_sec_idx;
            self.cur_sec_idx += 1;

            if let Some(range) = user_data_range {
                let seek_pos = u64::from(sector_idx) * SECTOR_BYTES;
                self.source.seek(SeekFrom::Start(seek_pos))?;
                self.source.read_exact(self.sec_buffer.as_mut())?;

                self.pos_in_user_data = range.start;
                self.len_of_user_data = range.end;
                return Ok(true);
            }
        }
    }
}


impl<R: Read + Seek> Read for UserDataStream<'_, R> {
    /// Reads user data bytes from the `UserDataStream` into the provided
    /// buffer.
    ///
    /// When the user data portion of the current sector is exhausted, or if
    /// no user data sector has been loaded yet, it calls
    /// `process_next_sector` to load the next sector that has user data.
    ///
    /// Reading continues until `buf` is full or the sector map has no more
    /// data sectors.
    ///
    /// # Returns
    ///
    /// The number of user data bytes read into `buf`. `0` means the end of
    /// the user data.
    ///
    /// # Errors
    ///
    /// Any error from `source`, including `UnexpectedEof` if the source is
    /// shorter than the sector map says.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_written_to_buf = 0;

        while bytes_written_to_buf < buf.len() {
            if self.pos_in_user_data >= self.len_of_user_data
                && !self.process_next_sector()?
            {
                break;
            }

            let source_slice = self.sec_buffer
                .get(self.pos_in_user_data..self.len_of_user_data)
                .unwrap_or_default();
            let dest_slice = buf
                .get_mut(bytes_written_to_buf..)
                .unwrap_or_default();
            let bytes_to_copy = source_slice.len().min(dest_slice.len());
            let (Some(src), Some(dst)) = (
                source_slice.get(..bytes_to_copy),
                dest_slice.get_mut(..bytes_to_copy),
            ) else {
                break;
            };
            dst.copy_from_slice(src);

            bytes_written_to_buf += bytes_to_copy;
            self.pos_in_user_data += bytes_to_copy;
        }

        Ok(bytes_written_to_buf)
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "the sizes and offsets in these tests are a few sectors"
)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    const SECTOR: usize = 2352;

    /// The buffer sizes every read test is run with: one byte, either side
    /// of a sector's user data, and larger than a sector.
    const BUFFER_SIZES: [usize; 5] = [1, 2047, 2048, 2049, 64 * 1024];

    /// `len` bytes that don't repeat with a period that divides a sector
    /// or a file length, so an offset error shows up as different bytes.
    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i % 251) as u8 ^ seed)
            .collect()
    }

    /// Reads `reader` to the end with `read` calls on a `buf_size` buffer,
    /// so each test controls the size of every read.
    fn read_with(reader: &mut impl Read, buf_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0; buf_size];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// Reads exactly `len` bytes from the current position.
    fn read_len(reader: &mut impl Read, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        reader.read_exact(&mut out).unwrap();
        out
    }

    // ---- MultiBinStream ----

    /// Three files whose lengths aren't multiples of a sector, and the
    /// bytes of all three joined.
    struct Bins {
        /// Keeps the files until the test ends.
        dir: tempfile::TempDir,
        paths: Vec<PathBuf>,
        joined: Vec<u8>,
    }

    fn bins(lengths: &[usize]) -> Bins {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        let mut joined = Vec::new();
        for (i, &len) in lengths.iter().enumerate() {
            let bytes = pattern(len, i as u8 * 0x40);
            let path = dir.path().join(format!("track{i}.bin"));
            File::create(&path).unwrap().write_all(&bytes).unwrap();
            paths.push(path);
            joined.extend_from_slice(&bytes);
        }
        Bins { dir, paths, joined }
    }

    const LENGTHS: [usize; 3] = [3 * SECTOR + 100, SECTOR - 1, 2 * SECTOR + 7];

    #[test]
    fn multi_bin_reads_the_files_joined_together() {
        let bins = bins(&LENGTHS);
        for buf_size in BUFFER_SIZES {
            let mut stream = MultiBinStream::new(bins.paths.clone()).unwrap();
            assert_eq!(
                read_with(&mut stream, buf_size),
                bins.joined,
                "buffer size {buf_size}"
            );
        }
    }

    #[test]
    fn multi_bin_skips_an_empty_file() {
        let bins = bins(&[SECTOR + 5, 0, 300]);
        let mut stream = MultiBinStream::new(bins.paths).unwrap();
        assert_eq!(read_with(&mut stream, 4096), bins.joined);
    }

    #[test]
    fn multi_bin_with_no_files_is_empty() {
        let mut stream = MultiBinStream::new(Vec::new()).unwrap();
        assert_eq!(stream.seek(SeekFrom::End(0)).unwrap(), 0);
        assert_eq!(read_with(&mut stream, 16), b"");
    }

    #[test]
    fn multi_bin_seeks_across_file_boundaries() {
        let bins = bins(&LENGTHS);
        let joined = &bins.joined;
        let first_end = LENGTHS[0] as u64;
        let second_end = first_end + LENGTHS[1] as u64;
        let mut stream = MultiBinStream::new(bins.paths.clone()).unwrap();

        // From the start, ending in the second file.
        assert_eq!(stream.seek(SeekFrom::Start(first_end - 10)).unwrap(),
            first_end - 10);
        assert_eq!(read_len(&mut stream, 20),
            joined[first_end as usize - 10..first_end as usize + 10]);

        // Forward from there, past the whole second file.
        let pos = stream.seek(SeekFrom::Current(LENGTHS[1] as i64)).unwrap();
        assert_eq!(pos, second_end + 10);
        assert_eq!(read_len(&mut stream, 5),
            joined[pos as usize..pos as usize + 5]);

        // Backward from there, into the first file.
        let pos = stream.seek(SeekFrom::Current(-(LENGTHS[1] as i64) - 100))
            .unwrap();
        assert_eq!(pos, first_end - 85);
        assert_eq!(read_len(&mut stream, 200),
            joined[pos as usize..pos as usize + 200]);

        // From the end, starting in the second file.
        let back = LENGTHS[2] as i64 + 3;
        let pos = stream.seek(SeekFrom::End(-back)).unwrap();
        assert_eq!(pos, second_end - 3);
        assert_eq!(read_with(&mut stream, 64), joined[pos as usize..]);
    }

    #[test]
    fn multi_bin_reads_nothing_past_the_end() {
        let bins = bins(&LENGTHS);
        let len = bins.joined.len() as u64;
        let mut stream = MultiBinStream::new(bins.paths.clone()).unwrap();

        assert_eq!(stream.seek(SeekFrom::End(0)).unwrap(), len);
        assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);

        // Seeking past the end is allowed, and a read there returns 0.
        assert_eq!(stream.seek(SeekFrom::Start(len + 1000)).unwrap(),
            len + 1000);
        assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);

        // The stream still reads after coming back.
        stream.seek(SeekFrom::Start(1)).unwrap();
        assert_eq!(read_len(&mut stream, 3), bins.joined[1..4]);
    }

    #[test]
    fn multi_bin_rejects_a_seek_before_the_start() {
        let bins = bins(&LENGTHS);
        let mut stream = MultiBinStream::new(bins.paths.clone()).unwrap();
        stream.seek(SeekFrom::Start(10)).unwrap();

        let err = stream.seek(SeekFrom::Current(-11)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = stream.seek(SeekFrom::End(-(bins.joined.len() as i64) - 1))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // A failed seek leaves the position where it was.
        assert_eq!(stream.stream_position().unwrap(), 10);
    }

    #[test]
    fn multi_bin_seeks_anywhere_up_to_u64_max() {
        let bins = bins(&LENGTHS);
        let len = bins.joined.len() as u64;
        let mut stream = MultiBinStream::new(bins.paths.clone()).unwrap();

        assert_eq!(stream.seek(SeekFrom::End(i64::MAX)).unwrap(),
            len + i64::MAX as u64);
        assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);

        assert_eq!(stream.seek(SeekFrom::Start(u64::MAX)).unwrap(), u64::MAX);
        let err = stream.seek(SeekFrom::Current(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(stream.stream_position().unwrap(), u64::MAX);
    }

    #[test]
    fn multi_bin_fails_for_a_missing_file() {
        let bins = bins(&LENGTHS);
        let mut paths = bins.paths.clone();
        paths.push(bins.dir.path().join("missing.bin"));
        assert!(MultiBinStream::new(paths).is_err());
    }

    // ---- SectorRegionStream ----

    /// A source of `sectors` sectors, each with different bytes.
    fn disc(sectors: usize) -> Vec<u8> {
        (0..sectors)
            .flat_map(|i| pattern(SECTOR, i as u8))
            .collect()
    }

    #[test]
    fn region_reads_exactly_its_sectors() {
        let bytes = disc(8);
        for buf_size in BUFFER_SIZES {
            let mut source = Cursor::new(bytes.clone());
            let mut region = SectorRegionStream::new(&mut source, 2, 3);
            assert_eq!(
                read_with(&mut region, buf_size),
                bytes[2 * SECTOR..5 * SECTOR],
                "buffer size {buf_size}"
            );
        }
    }

    #[test]
    fn region_read_to_end_keeps_the_last_bytes() {
        // `read_to_end` grows its buffer, so its last read asks for more
        // than is left.
        let bytes = disc(20);
        let mut source = Cursor::new(bytes.clone());
        let mut region = SectorRegionStream::new(&mut source, 1, 16);
        let mut out = Vec::new();
        region.read_to_end(&mut out).unwrap();
        assert_eq!(out, bytes[SECTOR..17 * SECTOR]);
    }

    #[test]
    fn region_seeks_within_the_region() {
        let bytes = disc(8);
        let base = 2 * SECTOR;
        let mut source = Cursor::new(bytes.clone());
        let mut region = SectorRegionStream::new(&mut source, 2, 3);

        region.seek(SeekFrom::Start(SECTOR as u64 - 4)).unwrap();
        assert_eq!(read_len(&mut region, 8),
            bytes[base + SECTOR - 4..base + SECTOR + 4]);

        let pos = region.seek(SeekFrom::Current(-(SECTOR as i64))).unwrap();
        assert_eq!(pos, 4);
        assert_eq!(read_len(&mut region, 4), bytes[base + 4..base + 8]);

        let pos = region.seek(SeekFrom::End(-10)).unwrap();
        assert_eq!(pos, 3 * SECTOR as u64 - 10);
        assert_eq!(read_with(&mut region, 4096),
            bytes[5 * SECTOR - 10..5 * SECTOR]);

        // Past the end reads nothing; before the start is an error.
        region.seek(SeekFrom::End(5)).unwrap();
        assert_eq!(region.read(&mut [0; 8]).unwrap(), 0);
        let err = region.seek(SeekFrom::Current(-(4 * SECTOR as i64)))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        region.seek(SeekFrom::Start(u64::MAX)).unwrap();
        let err = region.seek(SeekFrom::Current(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn region_past_the_source_is_an_error() {
        let mut source = Cursor::new(disc(3));
        let mut region = SectorRegionStream::new(&mut source, 2, 2);
        let mut out = Vec::new();
        let err = region.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    // ---- UserDataStream ----

    /// Where a sector type's user data starts in the raw sector, or `None`
    /// for types the data stream doesn't store (audio is kept as FLAC).
    fn user_data_start(sector_type: SectorType) -> Option<usize> {
        match sector_type {
            SectorType::Mode1 | SectorType::Mode1Exception
            | SectorType::PregapMode1 | SectorType::PregapMode1Exception => {
                Some(16)
            }
            SectorType::Mode2Form1 | SectorType::Mode2Form1Exception
            | SectorType::Mode2Form2 | SectorType::Mode2Form2Exception
            | SectorType::PregapMode2 | SectorType::PregapMode2Exception => {
                Some(24)
            }
            SectorType::ZeroedMode1Data | SectorType::ZeroedMode2Data => {
                Some(0)
            }
            SectorType::Audio | SectorType::PregapAudio
            | SectorType::ZeroedAudio | SectorType::None => None,
        }
    }

    /// The user data bytes for `map`, taken a sector at a time: each
    /// data sector contributes `data_size()` bytes from where its user
    /// data starts. This is the layout `build_decoded_map` assumes.
    fn reference_user_data(raw: &[u8], map: &SectorMap) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &sector_type) in map.sectors.iter().enumerate() {
            if let Some(start) = user_data_start(sector_type) {
                let start = i * SECTOR + start;
                let len = usize::from(sector_type.data_size());
                out.extend_from_slice(&raw[start..start + len]);
            }
        }
        out
    }

    /// Every sector type, with audio between data runs and a skipped
    /// sector first and last.
    fn mixed_map() -> SectorMap {
        SectorMap {
            sectors: vec![
                SectorType::Audio,
                SectorType::Mode1,
                SectorType::Mode1Exception,
                SectorType::PregapMode1,
                SectorType::PregapMode1Exception,
                SectorType::PregapAudio,
                SectorType::Mode2Form1,
                SectorType::Mode2Form1Exception,
                SectorType::Mode2Form2,
                SectorType::Mode2Form2Exception,
                SectorType::ZeroedAudio,
                SectorType::PregapMode2,
                SectorType::PregapMode2Exception,
                SectorType::ZeroedMode1Data,
                SectorType::None,
                SectorType::ZeroedMode2Data,
                SectorType::Audio,
            ],
        }
    }

    #[test]
    fn user_data_matches_the_reference() {
        let map = mixed_map();
        let raw = disc(map.sectors.len());
        let expected = reference_user_data(&raw, &map);
        for buf_size in BUFFER_SIZES {
            let mut source = Cursor::new(raw.clone());
            let mut stream = UserDataStream::new(&mut source, &map);
            assert_eq!(
                read_with(&mut stream, buf_size),
                expected,
                "buffer size {buf_size}"
            );
        }
    }

    #[test]
    fn user_data_of_only_audio_is_empty() {
        let map = SectorMap {
            sectors: vec![SectorType::Audio, SectorType::PregapAudio],
        };
        let mut source = Cursor::new(disc(2));
        let mut stream = UserDataStream::new(&mut source, &map);
        assert_eq!(read_with(&mut stream, 64), b"");
        assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);
    }

    #[test]
    fn user_data_from_a_short_source_is_an_error() {
        // The map has a data sector the source doesn't. Stopping early
        // would store too little data without saying so.
        let map = SectorMap {
            sectors: vec![SectorType::Mode1; 3],
        };
        let mut source = Cursor::new(disc(2));
        let mut stream = UserDataStream::new(&mut source, &map);
        let mut out = Vec::new();
        let err = stream.read_to_end(&mut out).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn user_data_through_a_region_of_split_files() {
        // As compression reads a data run: a region of the joined files,
        // filtered to user data.
        let map = SectorMap {
            sectors: vec![SectorType::Mode2Form1; 4],
        };
        let bins = bins(&[2 * SECTOR + 1000, 3 * SECTOR - 1000]);
        let mut joined = MultiBinStream::new(bins.paths.clone()).unwrap();
        let mut region = SectorRegionStream::new(&mut joined, 1, 4);
        let raw_region = &bins.joined[SECTOR..5 * SECTOR];
        let expected = reference_user_data(raw_region, &map);
        for buf_size in BUFFER_SIZES {
            region.seek(SeekFrom::Start(0)).unwrap();
            let mut stream = UserDataStream::new(&mut region, &map);
            assert_eq!(
                read_with(&mut stream, buf_size),
                expected,
                "buffer size {buf_size}"
            );
        }
    }
}
