//! Fragmented MP4 (fMP4) segment generator for HLS.
//!
//! Generates ISO BMFF init segments (ftyp + moov with trex) and
//! media segments (moof + mdat) from parsed MP4 metadata.

use super::mp4_parse::{
    self, Mp4Metadata, TrackInfo, TrackType,
};
use super::PackagingError;

// ── Box writing helpers ──

fn write_u16_be(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn write_u32_be(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn write_u64_be(buf: &mut Vec<u8>, val: u64) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn write_i32_be(buf: &mut Vec<u8>, val: i32) {
    buf.extend_from_slice(&val.to_be_bytes());
}

/// Write a box: size(4) + fourcc(4) + content.
fn write_box(buf: &mut Vec<u8>, fourcc: &[u8; 4], content: &[u8]) {
    let size = (content.len() + 8) as u32;
    write_u32_be(buf, size);
    buf.extend_from_slice(fourcc);
    buf.extend_from_slice(content);
}

/// Build a box and return it as a Vec.
fn make_box(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(content.len() + 8);
    write_box(&mut buf, fourcc, content);
    buf
}

// ── Init segment generation ──

/// Generate an fMP4 initialization segment (ftyp + moov with mvex/trex).
///
/// Contains codec configuration but no media data.
pub fn generate_init_segment(metadata: &Mp4Metadata) -> Vec<u8> {
    let mut output = Vec::new();

    // ftyp box
    let ftyp = build_ftyp();
    output.extend_from_slice(&ftyp);

    // moov box
    let moov = build_init_moov(metadata);
    output.extend_from_slice(&moov);

    output
}

fn build_ftyp() -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(b"isom"); // major_brand
    write_u32_be(&mut content, 0x200); // minor_version
    content.extend_from_slice(b"isom"); // compatible_brands
    content.extend_from_slice(b"iso6");
    content.extend_from_slice(b"mp41");
    make_box(b"ftyp", &content)
}

fn build_init_moov(metadata: &Mp4Metadata) -> Vec<u8> {
    let mut moov_content = Vec::new();

    // mvhd
    let mvhd = build_mvhd(metadata.timescale);
    moov_content.extend_from_slice(&mvhd);

    // trak for each track
    for track in &metadata.tracks {
        let trak = build_init_trak(track);
        moov_content.extend_from_slice(&trak);
    }

    // mvex (movie extends box, required for fragmented MP4)
    let mut mvex_content = Vec::new();
    for track in &metadata.tracks {
        let trex = build_trex(track.track_id);
        mvex_content.extend_from_slice(&trex);
    }
    let mvex = make_box(b"mvex", &mvex_content);
    moov_content.extend_from_slice(&mvex);

    make_box(b"moov", &moov_content)
}

fn build_mvhd(timescale: u32) -> Vec<u8> {
    let mut content = Vec::new();
    write_u32_be(&mut content, 0); // version=0, flags=0
    write_u32_be(&mut content, 0); // creation_time
    write_u32_be(&mut content, 0); // modification_time
    write_u32_be(&mut content, timescale);
    write_u32_be(&mut content, 0); // duration=0 for fragmented
    write_u32_be(&mut content, 0x00010000); // rate = 1.0
    write_u16_be(&mut content, 0x0100); // volume = 1.0
    content.extend_from_slice(&[0u8; 10]); // reserved
    // Matrix (identity, 9 x u32)
    let identity_matrix: [u32; 9] = [
        0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000,
    ];
    for val in &identity_matrix {
        write_u32_be(&mut content, *val);
    }
    content.extend_from_slice(&[0u8; 24]); // pre_defined
    write_u32_be(&mut content, 0xFFFFFFFF); // next_track_ID
    make_box(b"mvhd", &content)
}

fn build_init_trak(track: &TrackInfo) -> Vec<u8> {
    let mut trak_content = Vec::new();

    // tkhd
    let tkhd = build_tkhd(track);
    trak_content.extend_from_slice(&tkhd);

    // mdia
    let mdia = build_init_mdia(track);
    trak_content.extend_from_slice(&mdia);

    make_box(b"trak", &trak_content)
}

fn build_tkhd(track: &TrackInfo) -> Vec<u8> {
    let mut content = Vec::new();
    // version=0, flags=0x000003 (track_enabled | track_in_movie)
    write_u32_be(&mut content, 0x00000003);
    write_u32_be(&mut content, 0); // creation_time
    write_u32_be(&mut content, 0); // modification_time
    write_u32_be(&mut content, track.track_id);
    write_u32_be(&mut content, 0); // reserved
    write_u32_be(&mut content, 0); // duration=0 for fragmented
    content.extend_from_slice(&[0u8; 8]); // reserved
    write_u16_be(&mut content, 0); // layer
    write_u16_be(&mut content, 0); // alternate_group
    // volume: 0x0100 for audio, 0 for video
    let volume: u16 = if track.track_type == TrackType::Audio {
        0x0100
    } else {
        0
    };
    write_u16_be(&mut content, volume);
    write_u16_be(&mut content, 0); // reserved
    // Matrix (identity)
    let identity_matrix: [u32; 9] = [
        0x00010000, 0, 0, 0, 0x00010000, 0, 0, 0, 0x40000000,
    ];
    for val in &identity_matrix {
        write_u32_be(&mut content, *val);
    }
    // Width/height as 16.16 fixed point
    let w = track.width.unwrap_or(0) << 16;
    let h = track.height.unwrap_or(0) << 16;
    write_u32_be(&mut content, w);
    write_u32_be(&mut content, h);
    make_box(b"tkhd", &content)
}

fn build_init_mdia(track: &TrackInfo) -> Vec<u8> {
    let mut mdia_content = Vec::new();

    // mdhd
    let mdhd = build_mdhd(track.timescale);
    mdia_content.extend_from_slice(&mdhd);

    // hdlr
    let handler = match track.track_type {
        TrackType::Video => b"vide",
        TrackType::Audio => b"soun",
        TrackType::Other => b"hint",
    };
    let hdlr = build_hdlr(handler);
    mdia_content.extend_from_slice(&hdlr);

    // minf
    let minf = build_init_minf(track);
    mdia_content.extend_from_slice(&minf);

    make_box(b"mdia", &mdia_content)
}

fn build_mdhd(timescale: u32) -> Vec<u8> {
    let mut content = Vec::new();
    write_u32_be(&mut content, 0); // version=0, flags=0
    write_u32_be(&mut content, 0); // creation_time
    write_u32_be(&mut content, 0); // modification_time
    write_u32_be(&mut content, timescale);
    write_u32_be(&mut content, 0); // duration=0 for fragmented
    write_u16_be(&mut content, 0x55C4); // language = "und"
    write_u16_be(&mut content, 0); // pre_defined
    make_box(b"mdhd", &content)
}

fn build_hdlr(handler: &[u8; 4]) -> Vec<u8> {
    let mut content = Vec::new();
    write_u32_be(&mut content, 0); // version=0, flags=0
    write_u32_be(&mut content, 0); // pre_defined
    content.extend_from_slice(handler);
    content.extend_from_slice(&[0u8; 12]); // reserved
    content.push(0); // name (null-terminated empty string)
    make_box(b"hdlr", &content)
}

fn build_init_minf(track: &TrackInfo) -> Vec<u8> {
    let mut minf_content = Vec::new();

    // Media header box (vmhd for video, smhd for audio)
    match track.track_type {
        TrackType::Video => {
            let mut vmhd = Vec::new();
            write_u32_be(&mut vmhd, 0x00000001); // version=0, flags=1
            write_u16_be(&mut vmhd, 0); // graphicsmode
            vmhd.extend_from_slice(&[0u8; 6]); // opcolor
            let vmhd_box = make_box(b"vmhd", &vmhd);
            minf_content.extend_from_slice(&vmhd_box);
        }
        TrackType::Audio => {
            let mut smhd = Vec::new();
            write_u32_be(&mut smhd, 0); // version=0, flags=0
            write_u16_be(&mut smhd, 0); // balance
            write_u16_be(&mut smhd, 0); // reserved
            let smhd_box = make_box(b"smhd", &smhd);
            minf_content.extend_from_slice(&smhd_box);
        }
        _ => {}
    }

    // dinf + dref (required, minimal)
    let mut dref_content = Vec::new();
    write_u32_be(&mut dref_content, 0); // version=0, flags=0
    write_u32_be(&mut dref_content, 1); // entry_count=1
    // url entry (self-contained)
    let mut url_content = Vec::new();
    write_u32_be(&mut url_content, 0x00000001); // version=0, flags=1 (self-contained)
    let url_box = make_box(b"url ", &url_content);
    dref_content.extend_from_slice(&url_box);
    let dref = make_box(b"dref", &dref_content);
    let dinf = make_box(b"dinf", &dref);
    minf_content.extend_from_slice(&dinf);

    // stbl (with stsd from original, empty stts/stsc/stsz/stco)
    let stbl = build_init_stbl(track);
    minf_content.extend_from_slice(&stbl);

    make_box(b"minf", &minf_content)
}

fn build_init_stbl(track: &TrackInfo) -> Vec<u8> {
    let mut stbl_content = Vec::new();

    // stsd: copy from original (contains codec config like SPS/PPS)
    if !track.stsd_data.is_empty() {
        let stsd = make_box(b"stsd", &track.stsd_data);
        stbl_content.extend_from_slice(&stsd);
    } else {
        // Minimal empty stsd
        let mut stsd = Vec::new();
        write_u32_be(&mut stsd, 0); // version + flags
        write_u32_be(&mut stsd, 0); // entry_count = 0
        let stsd_box = make_box(b"stsd", &stsd);
        stbl_content.extend_from_slice(&stsd_box);
    }

    // Empty stts
    let mut stts = Vec::new();
    write_u32_be(&mut stts, 0); // version + flags
    write_u32_be(&mut stts, 0); // entry_count = 0
    stbl_content.extend_from_slice(&make_box(b"stts", &stts));

    // Empty stsc
    let mut stsc = Vec::new();
    write_u32_be(&mut stsc, 0);
    write_u32_be(&mut stsc, 0);
    stbl_content.extend_from_slice(&make_box(b"stsc", &stsc));

    // Empty stsz
    let mut stsz = Vec::new();
    write_u32_be(&mut stsz, 0); // version + flags
    write_u32_be(&mut stsz, 0); // sample_size
    write_u32_be(&mut stsz, 0); // sample_count
    stbl_content.extend_from_slice(&make_box(b"stsz", &stsz));

    // Empty stco
    let mut stco = Vec::new();
    write_u32_be(&mut stco, 0);
    write_u32_be(&mut stco, 0);
    stbl_content.extend_from_slice(&make_box(b"stco", &stco));

    make_box(b"stbl", &stbl_content)
}

fn build_trex(track_id: u32) -> Vec<u8> {
    let mut content = Vec::new();
    write_u32_be(&mut content, 0); // version=0, flags=0
    write_u32_be(&mut content, track_id);
    write_u32_be(&mut content, 1); // default_sample_description_index
    write_u32_be(&mut content, 0); // default_sample_duration
    write_u32_be(&mut content, 0); // default_sample_size
    write_u32_be(&mut content, 0); // default_sample_flags
    make_box(b"trex", &content)
}

// ── Media segment generation ──

/// Generate an fMP4 media segment (moof + mdat) for the given segment index.
pub fn generate_media_segment(
    mp4_data: &[u8],
    metadata: &Mp4Metadata,
    segment_index: u32,
    segment_duration: f64,
) -> Result<Vec<u8>, PackagingError> {
    let mut moof_content = Vec::new();
    let mut mdat_payload = Vec::new();

    // mfhd (movie fragment header)
    let mfhd = build_mfhd(segment_index + 1);
    moof_content.extend_from_slice(&mfhd);

    // For each track, build traf
    for track in &metadata.tracks {
        let (traf, track_mdat) = build_traf(
            mp4_data,
            track,
            segment_index,
            segment_duration,
            mdat_payload.len(),
        )?;
        moof_content.extend_from_slice(&traf);
        mdat_payload.extend_from_slice(&track_mdat);
    }

    // Build final moof box to calculate data_offset
    let moof = make_box(b"moof", &moof_content);

    // Build mdat
    let mdat = make_box(b"mdat", &mdat_payload);

    // Now we need to fix data_offset in trun boxes.
    // data_offset = moof_size + mdat_header(8) - but actually it's
    // offset from moof start to mdat payload start = moof.len() + 8
    // We'll rebuild with correct offsets.
    let moof_size = moof.len() as u32;
    let mut final_moof_content = Vec::new();
    final_moof_content.extend_from_slice(&mfhd);

    let mut track_data_offset = 0u32;
    for track in &metadata.tracks {
        let (traf, _) = build_traf_with_offset(
            mp4_data,
            track,
            segment_index,
            segment_duration,
            moof_size + 8 + track_data_offset, // offset from moof start to this track's data in mdat
        )?;
        // Calculate this track's data size for next track's offset
        let (_, track_mdat) = build_traf(
            mp4_data,
            track,
            segment_index,
            segment_duration,
            0,
        )?;
        track_data_offset += track_mdat.len() as u32;
        final_moof_content.extend_from_slice(&traf);
    }

    let final_moof = make_box(b"moof", &final_moof_content);

    let mut output = Vec::new();
    output.extend_from_slice(&final_moof);
    output.extend_from_slice(&mdat);
    Ok(output)
}

fn build_mfhd(sequence_number: u32) -> Vec<u8> {
    let mut content = Vec::new();
    write_u32_be(&mut content, 0); // version=0, flags=0
    write_u32_be(&mut content, sequence_number);
    make_box(b"mfhd", &content)
}

/// Calculate the sample range for a segment.
fn segment_sample_range(
    track: &TrackInfo,
    segment_index: u32,
    segment_duration: f64,
) -> (u32, u32) {
    let total = mp4_parse::total_samples(&track.sample_table.stts);
    if total == 0 {
        return (0, 0);
    }

    let start_time = segment_index as f64 * segment_duration * track.timescale as f64;
    let end_time = (segment_index + 1) as f64 * segment_duration * track.timescale as f64;

    let mut start_sample = mp4_parse::dts_to_sample(
        &track.sample_table.stts,
        start_time as u64,
    );

    // Align to keyframe for video tracks
    if track.track_type == TrackType::Video {
        start_sample = mp4_parse::nearest_sync_sample(
            &track.sample_table.stss,
            start_sample,
        );
    }

    let mut end_sample = mp4_parse::dts_to_sample(
        &track.sample_table.stts,
        end_time as u64,
    );

    // For the last segment, include all remaining samples
    if end_sample >= total {
        end_sample = total;
    }

    // For non-last video segments, align end to next keyframe
    if track.track_type == TrackType::Video && end_sample < total {
        // Find next keyframe after end_sample
        if let Some(ref stss) = track.sample_table.stss {
            let target = end_sample + 1; // 0-based to 1-based
            match stss.binary_search(&target) {
                Ok(_) => {} // end_sample is already a keyframe boundary
                Err(pos) => {
                    if pos < stss.len() {
                        end_sample = stss[pos] - 1; // next keyframe (0-based)
                    } else {
                        end_sample = total;
                    }
                }
            }
        }
    }

    if start_sample >= end_sample {
        return (start_sample, start_sample);
    }

    (start_sample, end_sample)
}

fn build_traf(
    mp4_data: &[u8],
    track: &TrackInfo,
    segment_index: u32,
    segment_duration: f64,
    _data_offset_placeholder: usize,
) -> Result<(Vec<u8>, Vec<u8>), PackagingError> {
    build_traf_with_offset(mp4_data, track, segment_index, segment_duration, 0)
}

fn build_traf_with_offset(
    mp4_data: &[u8],
    track: &TrackInfo,
    segment_index: u32,
    segment_duration: f64,
    data_offset: u32,
) -> Result<(Vec<u8>, Vec<u8>), PackagingError> {
    let (start_sample, end_sample) =
        segment_sample_range(track, segment_index, segment_duration);
    let sample_count = end_sample.saturating_sub(start_sample);

    if sample_count == 0 {
        // Empty traf for this track in this segment
        let mut traf_content = Vec::new();
        let tfhd = build_tfhd(track.track_id);
        traf_content.extend_from_slice(&tfhd);
        return Ok((make_box(b"traf", &traf_content), Vec::new()));
    }

    let mut traf_content = Vec::new();

    // tfhd
    let tfhd = build_tfhd(track.track_id);
    traf_content.extend_from_slice(&tfhd);

    // tfdt (track fragment decode time)
    let base_dts = mp4_parse::sample_to_dts(
        &track.sample_table.stts,
        start_sample,
    );
    let tfdt = build_tfdt(base_dts);
    traf_content.extend_from_slice(&tfdt);

    // Collect sample info and data
    let mut sample_entries = Vec::new();
    let mut mdat_payload = Vec::new();

    for i in start_sample..end_sample {
        let idx = i as usize;
        let duration = get_sample_duration(&track.sample_table.stts, i);
        let size = if idx < track.sample_table.stsz.len() {
            track.sample_table.stsz[idx]
        } else {
            0
        };

        // Get composition time offset
        let cts_offset = get_cts_offset(&track.sample_table.ctts, i);

        // Determine if this is a sync sample
        let is_sync = match &track.sample_table.stss {
            None => true,
            Some(stss) => stss.contains(&(i + 1)), // stss is 1-based
        };

        // Sample flags
        let flags = if is_sync {
            0x02000000 // sample_depends_on=2 (does not depend on others)
        } else {
            0x01010000 // sample_depends_on=1 (depends on others) + is_non_sync
        };

        sample_entries.push((duration, size, flags, cts_offset));

        // Copy sample data from original MP4
        if let Some((offset, sz)) = mp4_parse::get_sample_offset(&track.sample_table, i) {
            let start = offset as usize;
            let end = start + sz as usize;
            if end <= mp4_data.len() {
                mdat_payload.extend_from_slice(&mp4_data[start..end]);
            }
        }
    }

    // trun
    let trun = build_trun(&sample_entries, data_offset);
    traf_content.extend_from_slice(&trun);

    Ok((make_box(b"traf", &traf_content), mdat_payload))
}

fn build_tfhd(track_id: u32) -> Vec<u8> {
    let mut content = Vec::new();
    // version=0, flags=0x020000 (default-base-is-moof)
    write_u32_be(&mut content, 0x00020000);
    write_u32_be(&mut content, track_id);
    make_box(b"tfhd", &content)
}

fn build_tfdt(base_media_decode_time: u64) -> Vec<u8> {
    let mut content = Vec::new();
    // version=1 for 64-bit time, flags=0
    write_u32_be(&mut content, 0x01000000);
    write_u64_be(&mut content, base_media_decode_time);
    make_box(b"tfdt", &content)
}

fn build_trun(
    samples: &[(u32, u32, u32, i32)], // (duration, size, flags, cts_offset)
    data_offset: u32,
) -> Vec<u8> {
    let mut content = Vec::new();
    // version=0, flags: data-offset-present(0x1) + sample-duration(0x100) +
    //   sample-size(0x200) + sample-flags(0x400) + sample-cts-offset(0x800)
    let flags: u32 = 0x00000F01;
    write_u32_be(&mut content, flags);
    write_u32_be(&mut content, samples.len() as u32);
    write_i32_be(&mut content, data_offset as i32); // data_offset

    for (duration, size, sample_flags, cts_offset) in samples {
        write_u32_be(&mut content, *duration);
        write_u32_be(&mut content, *size);
        write_u32_be(&mut content, *sample_flags);
        write_i32_be(&mut content, *cts_offset);
    }

    make_box(b"trun", &content)
}

/// Get the duration of a specific sample from stts.
fn get_sample_duration(stts: &[mp4_parse::SttsEntry], sample_index: u32) -> u32 {
    let mut remaining = sample_index;
    for entry in stts {
        if remaining < entry.sample_count {
            return entry.sample_delta;
        }
        remaining -= entry.sample_count;
    }
    // Fallback: use last entry's delta
    stts.last().map(|e| e.sample_delta).unwrap_or(0)
}

/// Get the composition time offset for a specific sample from ctts.
fn get_cts_offset(ctts: &Option<Vec<mp4_parse::CttsEntry>>, sample_index: u32) -> i32 {
    match ctts {
        None => 0,
        Some(entries) => {
            let mut remaining = sample_index;
            for entry in entries {
                if remaining < entry.sample_count {
                    return entry.sample_offset;
                }
                remaining -= entry.sample_count;
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::mp4_parse::{
        Mp4Metadata, SampleTable, StscEntry, SttsEntry, TrackInfo, TrackType,
    };

    fn make_test_metadata() -> Mp4Metadata {
        Mp4Metadata {
            duration_secs: 10.0,
            timescale: 1000,
            tracks: vec![TrackInfo {
                track_id: 1,
                track_type: TrackType::Video,
                codec: "avc1".into(),
                timescale: 90000,
                duration: 900000,
                sample_table: SampleTable {
                    stts: vec![SttsEntry {
                        sample_count: 300,
                        sample_delta: 3000, // 30fps at 90000 timescale
                    }],
                    stsc: vec![StscEntry {
                        first_chunk: 1,
                        samples_per_chunk: 30,
                        sample_description_index: 1,
                    }],
                    stsz: vec![1000; 300], // 1000 bytes each
                    stco: (0..10).map(|i| (i * 30000 + 1000) as u64).collect(),
                    stss: Some(vec![1, 31, 61, 91, 121, 151, 181, 211, 241, 271]),
                    ctts: None,
                },
                width: Some(1920),
                height: Some(1080),
                sample_rate: None,
                channels: None,
                stsd_data: vec![0; 8], // minimal stsd
            }],
        }
    }

    #[test]
    fn test_generate_init_segment() {
        let metadata = make_test_metadata();
        let init = generate_init_segment(&metadata);

        // Should start with ftyp
        assert!(init.len() > 8);
        assert_eq!(&init[4..8], b"ftyp");

        // Should contain moov
        let moov_pos = init
            .windows(4)
            .position(|w| w == b"moov")
            .expect("moov not found");
        assert!(moov_pos > 0);

        // Should contain trex (inside mvex)
        let trex_pos = init
            .windows(4)
            .position(|w| w == b"trex")
            .expect("trex not found");
        assert!(trex_pos > moov_pos);
    }

    #[test]
    fn test_generate_media_segment() {
        let metadata = make_test_metadata();
        // Create fake MP4 data large enough for sample offsets
        let mp4_data = vec![0xABu8; 400000];

        let segment = generate_media_segment(&mp4_data, &metadata, 0, 6.0).unwrap();

        // Should contain moof
        assert!(segment.windows(4).any(|w| w == b"moof"));
        // Should contain mdat
        assert!(segment.windows(4).any(|w| w == b"mdat"));
        // Should contain mfhd
        assert!(segment.windows(4).any(|w| w == b"mfhd"));
        // Should contain traf
        assert!(segment.windows(4).any(|w| w == b"traf"));
        // Should contain trun
        assert!(segment.windows(4).any(|w| w == b"trun"));
    }

    #[test]
    fn test_segment_sample_range() {
        let metadata = make_test_metadata();
        let track = &metadata.tracks[0];

        // First segment (0-6s at 90000 timescale)
        let (start, end) = segment_sample_range(track, 0, 6.0);
        assert_eq!(start, 0); // First keyframe
        assert!(end > start);

        // Second segment
        let (start2, _end2) = segment_sample_range(track, 1, 6.0);
        assert!(start2 > 0);
    }

    #[test]
    fn test_get_sample_duration() {
        let stts = vec![
            SttsEntry { sample_count: 5, sample_delta: 1000 },
            SttsEntry { sample_count: 5, sample_delta: 2000 },
        ];
        assert_eq!(get_sample_duration(&stts, 0), 1000);
        assert_eq!(get_sample_duration(&stts, 4), 1000);
        assert_eq!(get_sample_duration(&stts, 5), 2000);
        assert_eq!(get_sample_duration(&stts, 9), 2000);
    }

    #[test]
    fn test_build_ftyp() {
        let ftyp = build_ftyp();
        assert_eq!(&ftyp[4..8], b"ftyp");
        assert_eq!(&ftyp[8..12], b"isom");
    }

    #[test]
    fn test_build_trex() {
        let trex = build_trex(1);
        assert_eq!(&trex[4..8], b"trex");
        // track_id at offset 12
        assert_eq!(u32::from_be_bytes([trex[12], trex[13], trex[14], trex[15]]), 1);
    }

    #[test]
    fn test_empty_segment() {
        let metadata = Mp4Metadata {
            duration_secs: 0.0,
            timescale: 1000,
            tracks: vec![TrackInfo {
                track_id: 1,
                track_type: TrackType::Video,
                codec: "avc1".into(),
                timescale: 90000,
                duration: 0,
                sample_table: SampleTable {
                    stts: vec![],
                    stsc: vec![],
                    stsz: vec![],
                    stco: vec![],
                    stss: None,
                    ctts: None,
                },
                width: Some(320),
                height: Some(240),
                sample_rate: None,
                channels: None,
                stsd_data: vec![],
            }],
        };

        let result = generate_media_segment(&[], &metadata, 0, 6.0);
        assert!(result.is_ok());
    }
}
