use crate::error::IngestError;

/// AAC codec configuration extracted from AudioSpecificConfig.
#[derive(Debug, Clone)]
pub struct AacConfig {
    /// Raw AudioSpecificConfig bytes (typically 2 bytes).
    pub audio_specific_config: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Track timescale (= sample_rate for AAC).
    pub timescale: u32,
}

/// Standard AAC sampling frequency table (ISO 14496-3).
const AAC_SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

impl AacConfig {
    /// Parse from AudioSpecificConfig (2+ bytes from RTMP AAC sequence header or ADTS).
    ///
    /// Format: audioObjectType(5 bits) + samplingFrequencyIndex(4 bits) +
    ///         channelConfiguration(4 bits) + ...
    pub fn from_audio_specific_config(data: &[u8]) -> Result<Self, IngestError> {
        if data.len() < 2 {
            return Err(IngestError::Codec("AudioSpecificConfig too short".into()));
        }

        // audioObjectType: 5 bits
        let _aot = (data[0] >> 3) & 0x1F;

        // samplingFrequencyIndex: 4 bits (3 bits from data[0], 1 bit from data[1])
        let freq_index = ((data[0] & 0x07) << 1) | ((data[1] >> 7) & 0x01);

        let sample_rate = if (freq_index as usize) < AAC_SAMPLE_RATES.len() {
            AAC_SAMPLE_RATES[freq_index as usize]
        } else if freq_index == 0x0F {
            // Explicit 24-bit frequency follows — need at least 5 bytes total
            if data.len() < 5 {
                return Err(IngestError::Codec(
                    "AudioSpecificConfig too short for explicit frequency".into(),
                ));
            }
            let freq = ((data[1] as u32 & 0x7F) << 17)
                | ((data[2] as u32) << 9)
                | ((data[3] as u32) << 1)
                | ((data[4] as u32) >> 7);
            freq
        } else {
            return Err(IngestError::Codec(format!(
                "invalid sampling frequency index: {}",
                freq_index
            )));
        };

        // channelConfiguration: 4 bits
        let channels = if freq_index == 0x0F {
            // After explicit frequency
            if data.len() < 5 {
                2 // default stereo
            } else {
                ((data[4] >> 3) & 0x0F) as u16
            }
        } else {
            ((data[1] >> 3) & 0x0F) as u16
        };

        let channels = if channels == 0 { 2 } else { channels }; // 0 = program_config_element, default to stereo

        Ok(Self {
            audio_specific_config: data.to_vec(),
            sample_rate,
            channels,
            timescale: sample_rate,
        })
    }

    /// Build the stsd box data for an fMP4 init segment.
    ///
    /// Returns: version(4) + entry_count(4) + mp4a sample entry box.
    pub fn build_stsd_data(&self) -> Vec<u8> {
        let mut stsd = Vec::new();

        // version=0, flags=0
        stsd.extend_from_slice(&0u32.to_be_bytes());
        // entry_count = 1
        stsd.extend_from_slice(&1u32.to_be_bytes());

        // mp4a sample entry
        let mp4a = self.build_mp4a_entry();
        stsd.extend_from_slice(&mp4a);

        stsd
    }

    fn build_mp4a_entry(&self) -> Vec<u8> {
        // Build esds box
        let esds = self.build_esds();
        let esds_box = make_box(b"esds", &esds);

        // mp4a sample entry
        let mut entry = Vec::new();
        // reserved (6 bytes)
        entry.extend_from_slice(&[0u8; 6]);
        // data_reference_index = 1
        entry.extend_from_slice(&1u16.to_be_bytes());
        // reserved (8 bytes)
        entry.extend_from_slice(&[0u8; 8]);
        // channelcount
        entry.extend_from_slice(&self.channels.to_be_bytes());
        // samplesize = 16
        entry.extend_from_slice(&16u16.to_be_bytes());
        // pre_defined = 0
        entry.extend_from_slice(&0u16.to_be_bytes());
        // reserved
        entry.extend_from_slice(&0u16.to_be_bytes());
        // samplerate as 16.16 fixed point
        entry.extend_from_slice(&(self.sample_rate as u32).to_be_bytes());
        entry.extend_from_slice(&0u16.to_be_bytes()); // fractional part
                                                      // esds box
        entry.extend_from_slice(&esds_box);

        make_box(b"mp4a", &entry)
    }

    fn build_esds(&self) -> Vec<u8> {
        let asc = &self.audio_specific_config;

        // DecoderSpecificInfo descriptor
        let dsi = build_descriptor(0x05, asc);

        // DecoderConfigDescriptor
        let mut dcd_content = Vec::new();
        dcd_content.push(0x40); // objectTypeIndication = Audio ISO/IEC 14496-3
        dcd_content.push(0x15); // streamType=5 (audio), upstream=0, reserved=1
        dcd_content.extend_from_slice(&[0x00, 0x00, 0x00]); // bufferSizeDB (3 bytes)
        dcd_content.extend_from_slice(&128000u32.to_be_bytes()); // maxBitrate
        dcd_content.extend_from_slice(&128000u32.to_be_bytes()); // avgBitrate
        dcd_content.extend_from_slice(&dsi);
        let dcd = build_descriptor(0x04, &dcd_content);

        // SLConfigDescriptor
        let slc = build_descriptor(0x06, &[0x02]); // predefined = 2

        // ES_Descriptor
        let mut esd_content = Vec::new();
        esd_content.extend_from_slice(&0u16.to_be_bytes()); // ES_ID
        esd_content.push(0x00); // streamDependenceFlag=0, URL_Flag=0, OCRstreamFlag=0, streamPriority=0
        esd_content.extend_from_slice(&dcd);
        esd_content.extend_from_slice(&slc);
        let esd = build_descriptor(0x03, &esd_content);

        // esds full box: version(4) + ES_Descriptor
        let mut esds = Vec::new();
        esds.extend_from_slice(&0u32.to_be_bytes()); // version=0, flags=0
        esds.extend_from_slice(&esd);
        esds
    }
}

/// Build an MPEG-4 descriptor: tag(1) + length(variable) + data.
fn build_descriptor(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut desc = Vec::new();
    desc.push(tag);

    // Length encoding: use extended form (4 bytes) for compatibility
    let len = data.len();
    desc.push(((len >> 21) & 0x7F | 0x80) as u8);
    desc.push(((len >> 14) & 0x7F | 0x80) as u8);
    desc.push(((len >> 7) & 0x7F | 0x80) as u8);
    desc.push((len & 0x7F) as u8);

    desc.extend_from_slice(data);
    desc
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

    #[test]
    fn test_parse_aac_lc_44100_stereo() {
        // AAC-LC, 44100 Hz, stereo: audioObjectType=2, freqIndex=4, channels=2
        // Binary: 00010 0100 0010 000 = 0x12 0x10
        let data = vec![0x12, 0x10];
        let config = AacConfig::from_audio_specific_config(&data).unwrap();
        assert_eq!(config.sample_rate, 44100);
        assert_eq!(config.channels, 2);
        assert_eq!(config.timescale, 44100);
    }

    #[test]
    fn test_parse_aac_lc_48000_stereo() {
        // AAC-LC, 48000 Hz, stereo: audioObjectType=2, freqIndex=3, channels=2
        // Binary: 00010 0011 0010 000 = 0x11 0x90
        let data = vec![0x11, 0x90];
        let config = AacConfig::from_audio_specific_config(&data).unwrap();
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
    }

    #[test]
    fn test_parse_aac_he_44100_mono() {
        // HE-AAC (SBR), 44100 Hz, mono: audioObjectType=5, freqIndex=4, channels=1
        // Binary: 00101 0100 0001 000 = 0x2A 0x08
        let data = vec![0x2A, 0x08];
        let config = AacConfig::from_audio_specific_config(&data).unwrap();
        assert_eq!(config.sample_rate, 44100);
        assert_eq!(config.channels, 1);
    }

    #[test]
    fn test_too_short() {
        let result = AacConfig::from_audio_specific_config(&[0x12]);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_stsd_data() {
        let data = vec![0x12, 0x10]; // AAC-LC 44100 stereo
        let config = AacConfig::from_audio_specific_config(&data).unwrap();
        let stsd = config.build_stsd_data();

        // Should have version(4) + entry_count(4) + mp4a box
        assert!(stsd.len() > 8);
        // entry_count = 1
        assert_eq!(u32::from_be_bytes([stsd[4], stsd[5], stsd[6], stsd[7]]), 1);
        // mp4a fourcc at offset 12
        assert_eq!(&stsd[12..16], b"mp4a");
    }

    #[test]
    fn test_build_descriptor() {
        let desc = build_descriptor(0x05, &[0x12, 0x10]);
        assert_eq!(desc[0], 0x05); // tag
                                   // Length bytes (extended form)
        assert_eq!(desc[4], 2); // actual length
        assert_eq!(&desc[5..], &[0x12, 0x10]); // data
    }
}
