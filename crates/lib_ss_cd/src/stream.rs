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


pub struct MultiBinStream {
    files: Vec<File>,
    file_boundaries: Vec<u64>,
    virtual_pos: u64,
    total_len: u64,
}

impl MultiBinStream {
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


pub struct UserDataStream<'a, R: Read + Seek> {
    source: &'a mut R,
    sector_map: &'a SectorMap,
    cur_sec_idx: u32,
    sec_buffer: Box<[u8; 2352]>,
    pos_in_user_data: usize,
    len_of_user_data: usize,
}


impl<'a, R: Read + Seek> UserDataStream<'a, R> {
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

    fn process_next_sector(&mut self) -> io::Result<()> {
        loop {
            if self.cur_sec_idx as usize >= self.sector_map.sectors.len() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }

            let sector_type = &self.sector_map.sectors[self.cur_sec_idx as usize];

            let user_data_range = match sector_type {
                SectorType::Mode1 => Some(16..2064),
                SectorType::Mode2Form1 | SectorType::Mode2Form1Exception =>
                    Some(24..2072),
                SectorType::Mode2Form2 => Some(24..2348),
                //Personal note, exceptions likely shouldn't be skipped.
                // Mode2Form1Exception is used for LibCrypt DRM and needs to be
                // retained with user_data.
                // TODO, potentially reassess this logic.

                // All other types (Audio, Pregap) are skipped.
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
