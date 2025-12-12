use crate::lib_structs::{
    DecodedSectorInfo, DiscManifest,
    ReconstructionContext, ReconstructionInfo, RleSectorMap, SectorType,
    StreamChunkInfo
};

impl<H: Copy + Eq> DiscManifest<H> {
    fn build_stream_info(
        &self,
        sector_info: &DecodedSectorInfo
    ) -> Option<ReconstructionInfo<H>> {
        let target_stream_offset = sector_info.stream_offset;

        let target_user_data_len = get_user_data_size(
            sector_info.sector_type
        );
        if target_user_data_len == 0 { return None; }

        let mut needed_chunks = Vec::new();
        let mut bytes_remaining_in_sector = target_user_data_len;
        let mut current_search_offset = target_stream_offset;
        let mut chunk_stream_offset = 0u64;

        for chunk_layout in &self.data_stream_layout {
            let chunk_start = chunk_stream_offset;
            let chunk_end = chunk_stream_offset + chunk_layout.uncomp_len as u64;

            if chunk_start < (
                current_search_offset + bytes_remaining_in_sector as u64
            ) && chunk_end > current_search_offset {
                let overlap_start_in_stream = current_search_offset
                    .max(chunk_start);

                let overlap_end_in_stream = (
                    target_stream_offset + target_user_data_len as u64
                    ).min(chunk_end);
                let overlap_len = (
                    overlap_end_in_stream - overlap_start_in_stream
                ) as u32;

                needed_chunks.push(StreamChunkInfo {
                    chunk_hash: chunk_layout.hash,
                    read_from_offset: (
                        overlap_start_in_stream - chunk_start
                    ) as u32,
                    read_length: overlap_len,
                });

                bytes_remaining_in_sector -= overlap_len;
                current_search_offset += overlap_len as u64;
                if bytes_remaining_in_sector == 0 {
                    break;
                }
            }

            chunk_stream_offset = chunk_end;
        }

        if needed_chunks.is_empty() {
            None
        } else {
            Some(ReconstructionInfo::FromStream { chunks: needed_chunks })
        }
    }
}

fn build_decoded_map(rle_map: &RleSectorMap) -> Vec<DecodedSectorInfo> {
    let total_sectors: usize = rle_map.runs
        .iter()
        .map(|(count, _)| *count as usize)
        .sum();

    let mut map = Vec::with_capacity(total_sectors);
    let mut current_stream_offset = 0u64;

    for (run_count, run_type) in &rle_map.runs {
        let user_data_size = get_user_data_size(*run_type) as u64;
        for _ in 0..*run_count {
            map.push(DecodedSectorInfo {
                sector_type: *run_type,
                stream_offset: current_stream_offset,
            });

            if user_data_size > 0 {
                current_stream_offset += user_data_size;
            }
        }
    }
    map
}



fn get_user_data_size(sector_type: SectorType) -> u32 {
    match sector_type {
        SectorType::Mode1 | SectorType::Mode2Form1 => 2048,
        SectorType::Mode2Form2 => 2324,
        _ => 0,
    }
}


impl<'a, H: Copy + Eq> ReconstructionContext<'a, H> {
    pub fn new(manifest: &'a DiscManifest<H>) -> Self {
        let decoded_map = build_decoded_map(&manifest.rle_sector_map);

        Self {manifest, decoded_map}
    }

    pub fn get_reconstruction_info(
        &self,
        target_sector: u32,
    ) -> Option<ReconstructionInfo<H>> {
        let sector_info = self.decoded_map.get(target_sector as usize)?;

        match sector_info.sector_type {
            SectorType::Audio | SectorType::Pregap => {
                for block in &self.manifest.block_map {
                    if target_sector >= block.start_sector &&
                        target_sector < (
                            block.start_sector + block.sector_count
                        )
                    {
                        let offset_in_block = (
                            target_sector - block.start_sector
                        ) * 2352;

                        return Some(ReconstructionInfo::FromBlock {
                            content_hash: block.content_hash,
                            offset_in_block,
                        });
                    }
                }
                None
            }
            SectorType::Mode1 | SectorType::Mode2Form1 |
                SectorType::Mode2Form2 =>
            {
                self.manifest.build_stream_info(
                    sector_info
                )
            }
            _ => Some(ReconstructionInfo::None),
        }
    }
}
