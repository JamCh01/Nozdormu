use crate::error::IngestError;

/// H.264 codec configuration extracted from SPS/PPS.
#[derive(Debug, Clone)]
pub struct H264Config {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub profile: u8,
    pub level: u8,
    /// Track timescale (typically 90000 for video).
    pub timescale: u32,
}

impl H264Config {
    /// Parse from AVCDecoderConfigurationRecord (RTMP sequence header body after AVC packet type).
    ///
    /// Format: configurationVersion(1) + AVCProfileIndication(1) + profile_compatibility(1)
    ///         + AVCLevelIndication(1) + lengthSizeMinusOne(1) + numSPS(1) + spsLength(2) + sps
    ///         + numPPS(1) + ppsLength(2) + pps
    pub fn from_avcc(data: &[u8]) -> Result<Self, IngestError> {
        if data.len() < 8 {
            return Err(IngestError::Codec(
                "AVCDecoderConfigurationRecord too short".into(),
            ));
        }

        let profile = data[1];
        let level = data[3];
        let num_sps = data[5] & 0x1F;
        if num_sps == 0 {
            return Err(IngestError::Codec("no SPS in AVCC".into()));
        }

        let mut offset = 6;
        if offset + 2 > data.len() {
            return Err(IngestError::Codec("AVCC truncated at SPS length".into()));
        }
        let sps_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + sps_len > data.len() {
            return Err(IngestError::Codec("AVCC truncated at SPS data".into()));
        }
        let sps = data[offset..offset + sps_len].to_vec();
        offset += sps_len;

        // Skip remaining SPS entries if any
        for _ in 1..num_sps {
            if offset + 2 > data.len() {
                break;
            }
            let len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2 + len;
        }

        if offset >= data.len() {
            return Err(IngestError::Codec("AVCC truncated at PPS count".into()));
        }
        let num_pps = data[offset];
        offset += 1;
        if num_pps == 0 {
            return Err(IngestError::Codec("no PPS in AVCC".into()));
        }

        if offset + 2 > data.len() {
            return Err(IngestError::Codec("AVCC truncated at PPS length".into()));
        }
        let pps_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + pps_len > data.len() {
            return Err(IngestError::Codec("AVCC truncated at PPS data".into()));
        }
        let pps = data[offset..offset + pps_len].to_vec();

        let (width, height) = parse_sps_dimensions(&sps)?;

        Ok(Self {
            sps,
            pps,
            width,
            height,
            profile,
            level,
            timescale: 90000,
        })
    }

    /// Parse from raw Annex-B SPS and PPS NAL units (SRT/TS path).
    pub fn from_annexb_sps_pps(sps: &[u8], pps: &[u8]) -> Result<Self, IngestError> {
        if sps.is_empty() {
            return Err(IngestError::Codec("empty SPS".into()));
        }
        if pps.is_empty() {
            return Err(IngestError::Codec("empty PPS".into()));
        }

        // SPS data is used as-is (NAL type check was redundant since both branches were identical)
        let sps_data = sps.to_vec();

        let profile = if sps_data.len() > 1 { sps_data[1] } else { 0 };
        let level = if sps_data.len() > 3 { sps_data[3] } else { 0 };

        let (width, height) = parse_sps_dimensions(&sps_data)?;

        Ok(Self {
            sps: sps_data,
            pps: pps.to_vec(),
            width,
            height,
            profile,
            level,
            timescale: 90000,
        })
    }

    /// Build the stsd box data for an fMP4 init segment.
    ///
    /// Returns: version(4) + entry_count(4) + avc1 sample entry box.
    pub fn build_stsd_data(&self) -> Vec<u8> {
        let mut stsd = Vec::new();

        // version=0, flags=0
        stsd.extend_from_slice(&0u32.to_be_bytes());
        // entry_count = 1
        stsd.extend_from_slice(&1u32.to_be_bytes());

        // avc1 sample entry
        let avc1 = self.build_avc1_entry();
        stsd.extend_from_slice(&avc1);

        stsd
    }

    fn build_avc1_entry(&self) -> Vec<u8> {
        // Build avcC box (AVCDecoderConfigurationRecord)
        let mut avcc_content = vec![
            1, // configurationVersion
            self.profile,
            0, // profile_compatibility
            self.level,
            0xFF, // lengthSizeMinusOne = 3 (4 bytes)
            0xE1, // numSPS = 1
        ];
        avcc_content.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.sps);
        avcc_content.push(1); // numPPS
        avcc_content.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        avcc_content.extend_from_slice(&self.pps);

        let avcc_box = make_box(b"avcC", &avcc_content);

        // avc1 sample entry
        let mut entry = Vec::new();
        // reserved (6 bytes)
        entry.extend_from_slice(&[0u8; 6]);
        // data_reference_index = 1
        entry.extend_from_slice(&1u16.to_be_bytes());
        // pre_defined + reserved (16 bytes)
        entry.extend_from_slice(&[0u8; 16]);
        // width
        entry.extend_from_slice(&(self.width as u16).to_be_bytes());
        // height
        entry.extend_from_slice(&(self.height as u16).to_be_bytes());
        // horizresolution = 72 dpi (0x00480000)
        entry.extend_from_slice(&0x00480000u32.to_be_bytes());
        // vertresolution = 72 dpi
        entry.extend_from_slice(&0x00480000u32.to_be_bytes());
        // reserved
        entry.extend_from_slice(&0u32.to_be_bytes());
        // frame_count = 1
        entry.extend_from_slice(&1u16.to_be_bytes());
        // compressorname (32 bytes, null-padded)
        entry.extend_from_slice(&[0u8; 32]);
        // depth = 0x0018
        entry.extend_from_slice(&0x0018u16.to_be_bytes());
        // pre_defined = -1
        entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
        // avcC box
        entry.extend_from_slice(&avcc_box);

        make_box(b"avc1", &entry)
    }
}

/// Parse SPS to extract width and height.
///
/// Uses h264-reader crate for robust parsing of the SPS NAL unit.
fn parse_sps_dimensions(sps: &[u8]) -> Result<(u32, u32), IngestError> {
    use h264_reader::nal::sps::SeqParameterSet;
    use h264_reader::rbsp::decode_nal;

    if sps.is_empty() {
        return Err(IngestError::Codec("empty SPS".into()));
    }

    // h264-reader expects the NAL unit without the start code but with the NAL header byte
    let rbsp =
        decode_nal(sps).map_err(|e| IngestError::Codec(format!("SPS RBSP decode: {:?}", e)))?;

    let parsed = SeqParameterSet::from_bits(h264_reader::rbsp::BitReader::new(&rbsp[..]))
        .map_err(|e| IngestError::Codec(format!("SPS parse: {:?}", e)))?;

    let width = (parsed.pic_width_in_mbs_minus1 + 1) * 16;
    let is_frame_mbs = matches!(
        parsed.frame_mbs_flags,
        h264_reader::nal::sps::FrameMbsFlags::Frames
    );
    let height = (parsed.pic_height_in_map_units_minus1 + 1) * 16 * (2 - is_frame_mbs as u32);

    // Apply cropping if present
    let (crop_left, crop_right, crop_top, crop_bottom) =
        if let Some(ref crop) = parsed.frame_cropping {
            let sub_width_c: u32 = match parsed.chroma_info.chroma_format {
                h264_reader::nal::sps::ChromaFormat::Monochrome => 1,
                h264_reader::nal::sps::ChromaFormat::YUV420 => 2,
                h264_reader::nal::sps::ChromaFormat::YUV422 => 2,
                h264_reader::nal::sps::ChromaFormat::YUV444 => 1,
                _ => 1,
            };
            let sub_height_c: u32 = match parsed.chroma_info.chroma_format {
                h264_reader::nal::sps::ChromaFormat::Monochrome => 1,
                h264_reader::nal::sps::ChromaFormat::YUV420 => 2,
                _ => 1,
            };
            let crop_unit_x = sub_width_c;
            let crop_unit_y = sub_height_c * (2 - is_frame_mbs as u32);
            (
                crop.left_offset * crop_unit_x,
                crop.right_offset * crop_unit_x,
                crop.top_offset * crop_unit_y,
                crop.bottom_offset * crop_unit_y,
            )
        } else {
            (0, 0, 0, 0)
        };

    let final_width = width - crop_left - crop_right;
    let final_height = height - crop_top - crop_bottom;

    Ok((final_width, final_height))
}

/// Build a box: size(4) + fourcc(4) + content.
fn make_box(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let size = (content.len() + 8) as u32;
    let mut buf = Vec::with_capacity(size as usize);
    buf.extend_from_slice(&size.to_be_bytes());
    buf.extend_from_slice(fourcc);
    buf.extend_from_slice(content);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal SPS for 1920x1080 (Baseline profile, level 4.0)
    // This is a simplified test — real SPS would be more complex
    fn make_test_avcc() -> Vec<u8> {
        // A minimal but valid AVCDecoderConfigurationRecord
        // with a synthetic SPS that h264-reader can parse
        let sps = make_minimal_sps();
        let pps = vec![0x68, 0xCE, 0x38, 0x80]; // minimal PPS

        let mut avcc = Vec::new();
        avcc.push(1); // configurationVersion
        avcc.push(sps[1]); // profile
        avcc.push(sps[2]); // profile_compat
        avcc.push(sps[3]); // level
        avcc.push(0xFF); // lengthSizeMinusOne = 3
        avcc.push(0xE1); // numSPS = 1
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1); // numPPS
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);
        avcc
    }

    fn make_minimal_sps() -> Vec<u8> {
        // SPS NAL unit for 320x240, Baseline profile, level 2.0
        // Generated from a real H.264 encoder (ffmpeg -s 320x240)
        // NAL header: 0x67 (forbidden=0, nal_ref_idc=3, nal_unit_type=7)
        vec![0x67, 0x42, 0xC0, 0x14, 0xD9, 0x00, 0xA0, 0x5B, 0x20]
    }

    #[test]
    fn test_from_avcc() {
        let avcc = make_test_avcc();
        let config = H264Config::from_avcc(&avcc).unwrap();
        assert_eq!(config.profile, 0x42); // Baseline
        assert!(config.width > 0);
        assert!(config.height > 0);
        assert!(!config.sps.is_empty());
        assert!(!config.pps.is_empty());
    }

    #[test]
    fn test_from_avcc_too_short() {
        let result = H264Config::from_avcc(&[0; 4]);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_stsd_data() {
        let avcc = make_test_avcc();
        let config = H264Config::from_avcc(&avcc).unwrap();
        let stsd = config.build_stsd_data();

        // Should have version(4) + entry_count(4) + avc1 box
        assert!(stsd.len() > 8);
        // entry_count = 1
        assert_eq!(u32::from_be_bytes([stsd[4], stsd[5], stsd[6], stsd[7]]), 1);
        // avc1 fourcc at offset 12 (after size at 8)
        assert_eq!(&stsd[12..16], b"avc1");
    }

    #[test]
    fn test_from_annexb_sps_pps() {
        let sps = make_minimal_sps();
        let pps = vec![0x68, 0xCE, 0x38, 0x80];
        let config = H264Config::from_annexb_sps_pps(&sps, &pps).unwrap();
        assert!(config.width > 0);
        assert!(config.height > 0);
    }

    #[test]
    fn test_empty_sps_rejected() {
        let result = H264Config::from_annexb_sps_pps(&[], &[0x68]);
        assert!(result.is_err());
    }
}
