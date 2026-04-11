//! MP4 atom parser — extracts metadata needed for HLS transmuxing.
//!
//! Parses moov/trak/stbl atoms to build `Mp4Metadata` with sample tables.
//! All parsing is from `&[u8]` with bounds checking. No unsafe code.

#[derive(Debug, thiserror::Error)]
pub enum Mp4Error {
    #[error("moov atom not found")]
    MoovNotFound,
    #[error("truncated atom at offset {0}")]
    Truncated(u64),
    #[error("no tracks found")]
    NoTracks,
    #[error("invalid atom: {0}")]
    InvalidAtom(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// Parsed MP4 file metadata.
#[derive(Debug, Clone)]
pub struct Mp4Metadata {
    pub duration_secs: f64,
    pub timescale: u32,
    pub tracks: Vec<TrackInfo>,
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub track_id: u32,
    pub track_type: TrackType,
    pub codec: String,
    pub timescale: u32,
    pub duration: u64,
    pub sample_table: SampleTable,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// Raw sample description box data (for init segment generation)
    pub stsd_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackType {
    Video,
    Audio,
    Other,
}

#[derive(Debug, Clone)]
pub struct SampleTable {
    pub stts: Vec<SttsEntry>,
    pub stsc: Vec<StscEntry>,
    pub stsz: Vec<u32>,
    pub stco: Vec<u64>,
    pub stss: Option<Vec<u32>>,
    pub ctts: Option<Vec<CttsEntry>>,
}

#[derive(Debug, Clone)]
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

#[derive(Debug, Clone)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub samples_per_chunk: u32,
    pub sample_description_index: u32,
}

#[derive(Debug, Clone)]
pub struct CttsEntry {
    pub sample_count: u32,
    pub sample_offset: i32,
}

// ── Byte reading helpers ──

fn read_u16(data: &[u8], offset: usize) -> Result<u16, Mp4Error> {
    if offset + 2 > data.len() {
        return Err(Mp4Error::Truncated(offset as u64));
    }
    Ok(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, Mp4Error> {
    if offset + 4 > data.len() {
        return Err(Mp4Error::Truncated(offset as u64));
    }
    Ok(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64, Mp4Error> {
    if offset + 8 > data.len() {
        return Err(Mp4Error::Truncated(offset as u64));
    }
    Ok(u64::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ]))
}

fn read_fourcc(data: &[u8], offset: usize) -> Result<[u8; 4], Mp4Error> {
    if offset + 4 > data.len() {
        return Err(Mp4Error::Truncated(offset as u64));
    }
    Ok([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn fourcc_str(cc: &[u8; 4]) -> String {
    String::from_utf8_lossy(cc).to_string()
}

// ── Atom iteration ──

struct AtomIter<'a> {
    data: &'a [u8],
    offset: usize,
    end: usize,
}

impl<'a> AtomIter<'a> {
    fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        Self {
            data,
            offset: start,
            end: end.min(data.len()),
        }
    }
}

struct Atom {
    fourcc: [u8; 4],
    #[allow(dead_code)]
    header_size: usize,
    data_start: usize,
    data_end: usize,
}

impl<'a> Iterator for AtomIter<'a> {
    type Item = Result<Atom, Mp4Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 8 > self.end {
            return None;
        }

        let size = match read_u32(self.data, self.offset) {
            Ok(s) => s as u64,
            Err(e) => return Some(Err(e)),
        };
        let fourcc = match read_fourcc(self.data, self.offset + 4) {
            Ok(cc) => cc,
            Err(e) => return Some(Err(e)),
        };

        let (header_size, atom_size) = if size == 1 {
            // Extended size (64-bit)
            if self.offset + 16 > self.end {
                return Some(Err(Mp4Error::Truncated(self.offset as u64)));
            }
            match read_u64(self.data, self.offset + 8) {
                Ok(ext) => (16, ext),
                Err(e) => return Some(Err(e)),
            }
        } else if size == 0 {
            // Atom extends to end of container
            (8, (self.end - self.offset) as u64)
        } else {
            (8, size)
        };

        let atom_end = self.offset + atom_size as usize;
        if atom_end > self.end {
            return Some(Err(Mp4Error::Truncated(self.offset as u64)));
        }

        let atom = Atom {
            fourcc,
            header_size,
            data_start: self.offset + header_size,
            data_end: atom_end,
        };

        self.offset = atom_end;
        Some(Ok(atom))
    }
}

// ── Main parser ──

/// Parse MP4 metadata from a byte buffer.
pub fn parse_mp4(data: &[u8]) -> Result<Mp4Metadata, Mp4Error> {
    // Find moov atom at top level
    let moov = find_atom(data, 0, data.len(), b"moov")?.ok_or(Mp4Error::MoovNotFound)?;

    // Parse mvhd for global timescale/duration
    let (timescale, duration) = parse_mvhd(data, &moov)?;

    let duration_secs = if timescale > 0 {
        duration as f64 / timescale as f64
    } else {
        0.0
    };

    // Parse each trak
    let mut tracks = Vec::new();
    for atom_result in AtomIter::new(data, moov.data_start, moov.data_end) {
        let atom = atom_result?;
        if &atom.fourcc == b"trak" {
            if let Some(track) = parse_trak(data, &atom)? {
                tracks.push(track);
            }
        }
    }

    if tracks.is_empty() {
        return Err(Mp4Error::NoTracks);
    }

    Ok(Mp4Metadata {
        duration_secs,
        timescale,
        tracks,
    })
}

fn find_atom(
    data: &[u8],
    start: usize,
    end: usize,
    target: &[u8; 4],
) -> Result<Option<Atom>, Mp4Error> {
    for atom_result in AtomIter::new(data, start, end) {
        let atom = atom_result?;
        if &atom.fourcc == target {
            return Ok(Some(atom));
        }
    }
    Ok(None)
}

fn parse_mvhd(data: &[u8], moov: &Atom) -> Result<(u32, u64), Mp4Error> {
    let mvhd = find_atom(data, moov.data_start, moov.data_end, b"mvhd")?
        .ok_or(Mp4Error::InvalidAtom("missing mvhd".into()))?;

    let d = &data[mvhd.data_start..mvhd.data_end];
    if d.is_empty() {
        return Err(Mp4Error::Truncated(mvhd.data_start as u64));
    }

    let version = d[0];
    if version == 1 {
        // Version 1: 8-byte fields
        if d.len() < 28 {
            return Err(Mp4Error::Truncated(mvhd.data_start as u64));
        }
        let timescale = read_u32(d, 20)?;
        let duration = read_u64(d, 24)?;
        Ok((timescale, duration))
    } else {
        // Version 0: 4-byte fields
        if d.len() < 20 {
            return Err(Mp4Error::Truncated(mvhd.data_start as u64));
        }
        let timescale = read_u32(d, 12)?;
        let duration = read_u32(d, 16)? as u64;
        Ok((timescale, duration))
    }
}

fn parse_trak(data: &[u8], trak: &Atom) -> Result<Option<TrackInfo>, Mp4Error> {
    // Parse tkhd
    let tkhd = find_atom(data, trak.data_start, trak.data_end, b"tkhd")?
        .ok_or(Mp4Error::InvalidAtom("missing tkhd".into()))?;
    let (track_id, width, height) = parse_tkhd(data, &tkhd)?;

    // Parse mdia
    let mdia = match find_atom(data, trak.data_start, trak.data_end, b"mdia")? {
        Some(a) => a,
        None => return Ok(None),
    };

    // Parse mdhd for timescale/duration
    let mdhd = find_atom(data, mdia.data_start, mdia.data_end, b"mdhd")?
        .ok_or(Mp4Error::InvalidAtom("missing mdhd".into()))?;
    let (timescale, duration) = parse_mdhd(data, &mdhd)?;

    // Parse hdlr for track type
    let hdlr = find_atom(data, mdia.data_start, mdia.data_end, b"hdlr")?
        .ok_or(Mp4Error::InvalidAtom("missing hdlr".into()))?;
    let track_type = parse_hdlr(data, &hdlr)?;

    // Parse minf/stbl
    let minf = match find_atom(data, mdia.data_start, mdia.data_end, b"minf")? {
        Some(a) => a,
        None => return Ok(None),
    };
    let stbl = match find_atom(data, minf.data_start, minf.data_end, b"stbl")? {
        Some(a) => a,
        None => return Ok(None),
    };

    // Parse sample table components
    let stsd_data = parse_stsd_raw(data, &stbl)?;
    let codec = parse_stsd_codec(data, &stbl)?;
    let stts = parse_stts(data, &stbl)?;
    let stsc = parse_stsc(data, &stbl)?;
    let stsz = parse_stsz(data, &stbl)?;
    let stco = parse_stco(data, &stbl)?;
    let stss = parse_stss(data, &stbl)?;
    let ctts = parse_ctts(data, &stbl)?;

    // Extract audio info from stsd if audio track
    let (sample_rate, channels) = if track_type == TrackType::Audio {
        parse_audio_info(data, &stbl)?
    } else {
        (None, None)
    };

    Ok(Some(TrackInfo {
        track_id,
        track_type,
        codec,
        timescale,
        duration,
        sample_table: SampleTable {
            stts,
            stsc,
            stsz,
            stco,
            stss,
            ctts,
        },
        width: if width > 0 { Some(width) } else { None },
        height: if height > 0 { Some(height) } else { None },
        sample_rate,
        channels,
        stsd_data,
    }))
}

fn parse_tkhd(data: &[u8], tkhd: &Atom) -> Result<(u32, u32, u32), Mp4Error> {
    let d = &data[tkhd.data_start..tkhd.data_end];
    if d.is_empty() {
        return Err(Mp4Error::Truncated(tkhd.data_start as u64));
    }
    let version = d[0];
    if version == 1 {
        if d.len() < 92 {
            return Err(Mp4Error::Truncated(tkhd.data_start as u64));
        }
        let track_id = read_u32(d, 20)?;
        // Width/height are 16.16 fixed-point at offset 84, 88
        let width = read_u32(d, 84)? >> 16;
        let height = read_u32(d, 88)? >> 16;
        Ok((track_id, width, height))
    } else {
        if d.len() < 80 {
            return Err(Mp4Error::Truncated(tkhd.data_start as u64));
        }
        let track_id = read_u32(d, 12)?;
        let width = read_u32(d, 72)? >> 16;
        let height = read_u32(d, 76)? >> 16;
        Ok((track_id, width, height))
    }
}

fn parse_mdhd(data: &[u8], mdhd: &Atom) -> Result<(u32, u64), Mp4Error> {
    let d = &data[mdhd.data_start..mdhd.data_end];
    if d.is_empty() {
        return Err(Mp4Error::Truncated(mdhd.data_start as u64));
    }
    let version = d[0];
    if version == 1 {
        if d.len() < 28 {
            return Err(Mp4Error::Truncated(mdhd.data_start as u64));
        }
        let timescale = read_u32(d, 20)?;
        let duration = read_u64(d, 24)?;
        Ok((timescale, duration))
    } else {
        if d.len() < 20 {
            return Err(Mp4Error::Truncated(mdhd.data_start as u64));
        }
        let timescale = read_u32(d, 12)?;
        let duration = read_u32(d, 16)? as u64;
        Ok((timescale, duration))
    }
}

fn parse_hdlr(data: &[u8], hdlr: &Atom) -> Result<TrackType, Mp4Error> {
    let d = &data[hdlr.data_start..hdlr.data_end];
    if d.len() < 12 {
        return Err(Mp4Error::Truncated(hdlr.data_start as u64));
    }
    // handler_type at offset 8 (after version + flags + pre_defined)
    let handler = read_fourcc(d, 8)?;
    match &handler {
        b"vide" => Ok(TrackType::Video),
        b"soun" => Ok(TrackType::Audio),
        _ => Ok(TrackType::Other),
    }
}

fn parse_stsd_raw(data: &[u8], stbl: &Atom) -> Result<Vec<u8>, Mp4Error> {
    match find_atom(data, stbl.data_start, stbl.data_end, b"stsd")? {
        Some(stsd) => Ok(data[stsd.data_start..stsd.data_end].to_vec()),
        None => Ok(Vec::new()),
    }
}

fn parse_stsd_codec(data: &[u8], stbl: &Atom) -> Result<String, Mp4Error> {
    let stsd = match find_atom(data, stbl.data_start, stbl.data_end, b"stsd")? {
        Some(a) => a,
        None => return Ok("unknown".into()),
    };
    let d = &data[stsd.data_start..stsd.data_end];
    // version(1) + flags(3) + entry_count(4) + first entry starts at offset 8
    // First entry: size(4) + fourcc(4)
    if d.len() < 16 {
        return Ok("unknown".into());
    }
    let codec_fourcc = read_fourcc(d, 12)?;
    Ok(fourcc_str(&codec_fourcc))
}

fn parse_stts(data: &[u8], stbl: &Atom) -> Result<Vec<SttsEntry>, Mp4Error> {
    let atom = match find_atom(data, stbl.data_start, stbl.data_end, b"stts")? {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let d = &data[atom.data_start..atom.data_end];
    if d.len() < 8 {
        return Err(Mp4Error::Truncated(atom.data_start as u64));
    }
    let entry_count = read_u32(d, 4)? as usize;
    // Cap allocation to what the data can actually hold (prevents OOM on malicious input)
    let max_possible = d.len().saturating_sub(8) / 8;
    let entry_count = entry_count.min(max_possible);
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let off = 8 + i * 8;
        if off + 8 > d.len() {
            return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
        }
        entries.push(SttsEntry {
            sample_count: read_u32(d, off)?,
            sample_delta: read_u32(d, off + 4)?,
        });
    }
    Ok(entries)
}

fn parse_stsc(data: &[u8], stbl: &Atom) -> Result<Vec<StscEntry>, Mp4Error> {
    let atom = match find_atom(data, stbl.data_start, stbl.data_end, b"stsc")? {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let d = &data[atom.data_start..atom.data_end];
    if d.len() < 8 {
        return Err(Mp4Error::Truncated(atom.data_start as u64));
    }
    let entry_count = read_u32(d, 4)? as usize;
    let max_possible = d.len().saturating_sub(8) / 12;
    let entry_count = entry_count.min(max_possible);
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let off = 8 + i * 12;
        if off + 12 > d.len() {
            return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
        }
        entries.push(StscEntry {
            first_chunk: read_u32(d, off)?,
            samples_per_chunk: read_u32(d, off + 4)?,
            sample_description_index: read_u32(d, off + 8)?,
        });
    }
    Ok(entries)
}

fn parse_stsz(data: &[u8], stbl: &Atom) -> Result<Vec<u32>, Mp4Error> {
    let atom = match find_atom(data, stbl.data_start, stbl.data_end, b"stsz")? {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let d = &data[atom.data_start..atom.data_end];
    if d.len() < 12 {
        return Err(Mp4Error::Truncated(atom.data_start as u64));
    }
    let sample_size = read_u32(d, 4)?;
    let sample_count = read_u32(d, 8)? as usize;

    if sample_size != 0 {
        // Fixed sample size — no per-sample entries in data, just repeat the value.
        // Cap to 10M samples as a safety limit against malicious input.
        let capped = sample_count.min(10_000_000);
        return Ok(vec![sample_size; capped]);
    }

    let max_possible = d.len().saturating_sub(12) / 4;
    let sample_count = sample_count.min(max_possible);
    let mut sizes = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let off = 12 + i * 4;
        if off + 4 > d.len() {
            return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
        }
        sizes.push(read_u32(d, off)?);
    }
    Ok(sizes)
}

fn parse_stco(data: &[u8], stbl: &Atom) -> Result<Vec<u64>, Mp4Error> {
    // Try co64 first (64-bit offsets), then stco (32-bit)
    if let Some(atom) = find_atom(data, stbl.data_start, stbl.data_end, b"co64")? {
        let d = &data[atom.data_start..atom.data_end];
        if d.len() < 8 {
            return Err(Mp4Error::Truncated(atom.data_start as u64));
        }
        let entry_count = read_u32(d, 4)? as usize;
        let max_possible = d.len().saturating_sub(8) / 8;
        let entry_count = entry_count.min(max_possible);
        let mut offsets = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let off = 8 + i * 8;
            if off + 8 > d.len() {
                return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
            }
            offsets.push(read_u64(d, off)?);
        }
        return Ok(offsets);
    }

    if let Some(atom) = find_atom(data, stbl.data_start, stbl.data_end, b"stco")? {
        let d = &data[atom.data_start..atom.data_end];
        if d.len() < 8 {
            return Err(Mp4Error::Truncated(atom.data_start as u64));
        }
        let entry_count = read_u32(d, 4)? as usize;
        let max_possible = d.len().saturating_sub(8) / 4;
        let entry_count = entry_count.min(max_possible);
        let mut offsets = Vec::with_capacity(entry_count);
        for i in 0..entry_count {
            let off = 8 + i * 4;
            if off + 4 > d.len() {
                return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
            }
            offsets.push(read_u32(d, off)? as u64);
        }
        return Ok(offsets);
    }

    Ok(Vec::new())
}

fn parse_stss(data: &[u8], stbl: &Atom) -> Result<Option<Vec<u32>>, Mp4Error> {
    let atom = match find_atom(data, stbl.data_start, stbl.data_end, b"stss")? {
        Some(a) => a,
        None => return Ok(None), // No stss = all samples are sync
    };
    let d = &data[atom.data_start..atom.data_end];
    if d.len() < 8 {
        return Err(Mp4Error::Truncated(atom.data_start as u64));
    }
    let entry_count = read_u32(d, 4)? as usize;
    let max_possible = d.len().saturating_sub(8) / 4;
    let entry_count = entry_count.min(max_possible);
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let off = 8 + i * 4;
        if off + 4 > d.len() {
            return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
        }
        entries.push(read_u32(d, off)?);
    }
    Ok(Some(entries))
}

fn parse_ctts(data: &[u8], stbl: &Atom) -> Result<Option<Vec<CttsEntry>>, Mp4Error> {
    let atom = match find_atom(data, stbl.data_start, stbl.data_end, b"ctts")? {
        Some(a) => a,
        None => return Ok(None),
    };
    let d = &data[atom.data_start..atom.data_end];
    if d.len() < 8 {
        return Err(Mp4Error::Truncated(atom.data_start as u64));
    }
    let entry_count = read_u32(d, 4)? as usize;
    let max_possible = d.len().saturating_sub(8) / 8;
    let entry_count = entry_count.min(max_possible);
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let off = 8 + i * 8;
        if off + 8 > d.len() {
            return Err(Mp4Error::Truncated(atom.data_start as u64 + off as u64));
        }
        entries.push(CttsEntry {
            sample_count: read_u32(d, off)?,
            sample_offset: read_u32(d, off + 4)? as i32,
        });
    }
    Ok(Some(entries))
}

fn parse_audio_info(data: &[u8], stbl: &Atom) -> Result<(Option<u32>, Option<u16>), Mp4Error> {
    let stsd = match find_atom(data, stbl.data_start, stbl.data_end, b"stsd")? {
        Some(a) => a,
        None => return Ok((None, None)),
    };
    let d = &data[stsd.data_start..stsd.data_end];
    // Audio sample entry starts at offset 8 (after version+flags+entry_count)
    // Audio entry: size(4) + fourcc(4) + reserved(6) + data_ref_index(2) +
    //              reserved(8) + channel_count(2) + sample_size(2) +
    //              pre_defined(2) + reserved(2) + sample_rate(4, 16.16 fixed)
    if d.len() < 36 {
        return Ok((None, None));
    }
    let channels = read_u16(d, 24)?;
    let sample_rate_fixed = read_u32(d, 32)?;
    let sample_rate = sample_rate_fixed >> 16;
    Ok((Some(sample_rate), Some(channels)))
}

// ── Utility functions used by fmp4_gen ──

/// Get the sample offset and size within the original MP4 data.
///
/// Uses stsc (sample-to-chunk) and stco (chunk offsets) to locate sample data.
pub fn get_sample_offset(table: &SampleTable, sample_index: u32) -> Option<(u64, u32)> {
    if sample_index as usize >= table.stsz.len() {
        return None;
    }
    let size = table.stsz[sample_index as usize];

    // Find which chunk this sample belongs to using stsc
    let (chunk_index, _sample_in_chunk) = sample_to_chunk(&table.stsc, sample_index)?;

    // Get chunk offset
    if chunk_index as usize >= table.stco.len() {
        return None;
    }
    let chunk_offset = table.stco[chunk_index as usize];

    // Calculate offset within chunk by summing sizes of preceding samples
    let first_sample_in_chunk = first_sample_of_chunk(&table.stsc, chunk_index)?;
    let mut intra_offset: u64 = 0;
    for i in first_sample_in_chunk..sample_index {
        if (i as usize) < table.stsz.len() {
            intra_offset += table.stsz[i as usize] as u64;
        }
    }

    Some((chunk_offset + intra_offset, size))
}

/// Map a sample index to (chunk_index, sample_index_within_chunk).
/// chunk_index is 0-based (stsc uses 1-based first_chunk).
fn sample_to_chunk(stsc: &[StscEntry], sample_index: u32) -> Option<(u32, u32)> {
    if stsc.is_empty() {
        return None;
    }

    let mut current_sample: u32 = 0;
    for i in 0..stsc.len() {
        let entry = &stsc[i];
        let next_first_chunk = if i + 1 < stsc.len() {
            stsc[i + 1].first_chunk
        } else {
            u32::MAX
        };

        let chunks_in_run = next_first_chunk.saturating_sub(entry.first_chunk);
        let samples_in_run = chunks_in_run.saturating_mul(entry.samples_per_chunk);

        if sample_index < current_sample + samples_in_run {
            let offset_in_run = sample_index - current_sample;
            let chunk_in_run = offset_in_run / entry.samples_per_chunk;
            let sample_in_chunk = offset_in_run % entry.samples_per_chunk;
            let chunk_index = (entry.first_chunk - 1) + chunk_in_run; // 0-based
            return Some((chunk_index, sample_in_chunk));
        }
        current_sample += samples_in_run;
    }
    None
}

/// Get the first sample index (0-based) of a given chunk (0-based).
fn first_sample_of_chunk(stsc: &[StscEntry], chunk_index: u32) -> Option<u32> {
    if stsc.is_empty() {
        return None;
    }

    let chunk_1based = chunk_index + 1;
    let mut current_sample: u32 = 0;

    for i in 0..stsc.len() {
        let entry = &stsc[i];
        let next_first_chunk = if i + 1 < stsc.len() {
            stsc[i + 1].first_chunk
        } else {
            u32::MAX
        };

        if chunk_1based >= entry.first_chunk && chunk_1based < next_first_chunk {
            let chunks_before = chunk_1based - entry.first_chunk;
            return Some(current_sample + chunks_before * entry.samples_per_chunk);
        }

        let chunks_in_run = next_first_chunk.saturating_sub(entry.first_chunk);
        current_sample += chunks_in_run * entry.samples_per_chunk;
    }
    None
}

/// Convert a sample index to its decode timestamp (in track timescale units).
pub fn sample_to_dts(stts: &[SttsEntry], sample_index: u32) -> u64 {
    let mut dts: u64 = 0;
    let mut remaining = sample_index;
    for entry in stts {
        if remaining <= entry.sample_count {
            dts += remaining as u64 * entry.sample_delta as u64;
            break;
        }
        dts += entry.sample_count as u64 * entry.sample_delta as u64;
        remaining -= entry.sample_count;
    }
    dts
}

/// Find the sample index for a given decode timestamp.
/// Returns the sample index whose DTS is <= the given timestamp.
pub fn dts_to_sample(stts: &[SttsEntry], target_dts: u64) -> u32 {
    let mut dts: u64 = 0;
    let mut sample: u32 = 0;
    for entry in stts {
        let run_duration = entry.sample_count as u64 * entry.sample_delta as u64;
        if dts + run_duration > target_dts {
            if entry.sample_delta > 0 {
                let offset = ((target_dts - dts) / entry.sample_delta as u64) as u32;
                return sample + offset;
            }
            return sample;
        }
        dts += run_duration;
        sample += entry.sample_count;
    }
    sample
}

/// Find the nearest sync sample (keyframe) at or before the given sample index.
/// If stss is None, all samples are sync.
pub fn nearest_sync_sample(stss: &Option<Vec<u32>>, sample_index: u32) -> u32 {
    match stss {
        None => sample_index, // All samples are sync
        Some(sync_samples) => {
            // stss entries are 1-based
            let target = sample_index + 1;
            // Binary search for the largest sync sample <= target
            match sync_samples.binary_search(&target) {
                Ok(_) => sample_index,
                Err(pos) => {
                    if pos > 0 {
                        sync_samples[pos - 1] - 1 // Convert back to 0-based
                    } else {
                        0
                    }
                }
            }
        }
    }
}

/// Total number of samples in the track.
pub fn total_samples(stts: &[SttsEntry]) -> u32 {
    stts.iter().map(|e| e.sample_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: build a minimal valid MP4 ──

    fn write_u32(buf: &mut Vec<u8>, val: u32) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u64(buf: &mut Vec<u8>, val: u64) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    #[allow(dead_code)]
    fn write_u16(buf: &mut Vec<u8>, val: u16) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn make_atom(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let size = (content.len() + 8) as u32;
        let mut buf = Vec::new();
        write_u32(&mut buf, size);
        buf.extend_from_slice(fourcc);
        buf.extend_from_slice(content);
        buf
    }

    fn make_mvhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        let mut content = vec![0u8; 100]; // version=0, flags=0, then fields
                                          // version(1) + flags(3) + creation_time(4) + modification_time(4) = 12
        content[12] = (timescale >> 24) as u8;
        content[13] = (timescale >> 16) as u8;
        content[14] = (timescale >> 8) as u8;
        content[15] = timescale as u8;
        content[16] = (duration >> 24) as u8;
        content[17] = (duration >> 16) as u8;
        content[18] = (duration >> 8) as u8;
        content[19] = duration as u8;
        make_atom(b"mvhd", &content)
    }

    fn make_tkhd_v0(track_id: u32, width: u32, height: u32) -> Vec<u8> {
        let mut content = vec![0u8; 80];
        // track_id at offset 12
        content[12] = (track_id >> 24) as u8;
        content[13] = (track_id >> 16) as u8;
        content[14] = (track_id >> 8) as u8;
        content[15] = track_id as u8;
        // width at offset 72 (16.16 fixed point)
        let w = width << 16;
        content[72] = (w >> 24) as u8;
        content[73] = (w >> 16) as u8;
        content[74] = (w >> 8) as u8;
        content[75] = w as u8;
        // height at offset 76
        let h = height << 16;
        content[76] = (h >> 24) as u8;
        content[77] = (h >> 16) as u8;
        content[78] = (h >> 8) as u8;
        content[79] = h as u8;
        make_atom(b"tkhd", &content)
    }

    fn make_mdhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        let mut content = vec![0u8; 24];
        content[12] = (timescale >> 24) as u8;
        content[13] = (timescale >> 16) as u8;
        content[14] = (timescale >> 8) as u8;
        content[15] = timescale as u8;
        content[16] = (duration >> 24) as u8;
        content[17] = (duration >> 16) as u8;
        content[18] = (duration >> 8) as u8;
        content[19] = duration as u8;
        make_atom(b"mdhd", &content)
    }

    fn make_hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut content = vec![0u8; 24];
        // handler_type at offset 8
        content[8..12].copy_from_slice(handler);
        make_atom(b"hdlr", &content)
    }

    fn make_stsd_video() -> Vec<u8> {
        let mut content = vec![0u8; 8]; // version + flags + entry_count=1
        content[7] = 1; // entry_count = 1
                        // Sample entry: size(4) + fourcc(4) + rest
        let mut entry = vec![0u8; 86]; // minimal video sample entry
        let entry_size = (entry.len() + 8) as u32;
        let mut entry_with_header = Vec::new();
        write_u32(&mut entry_with_header, entry_size);
        entry_with_header.extend_from_slice(b"avc1");
        entry_with_header.append(&mut entry);
        content.extend_from_slice(&entry_with_header);
        make_atom(b"stsd", &content)
    }

    fn make_stts(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut content = vec![0u8; 4]; // version + flags
        write_u32(&mut content, entries.len() as u32);
        for (count, delta) in entries {
            write_u32(&mut content, *count);
            write_u32(&mut content, *delta);
        }
        make_atom(b"stts", &content)
    }

    fn make_stsc(entries: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut content = vec![0u8; 4];
        write_u32(&mut content, entries.len() as u32);
        for (first, spc, sdi) in entries {
            write_u32(&mut content, *first);
            write_u32(&mut content, *spc);
            write_u32(&mut content, *sdi);
        }
        make_atom(b"stsc", &content)
    }

    fn make_stsz(sizes: &[u32]) -> Vec<u8> {
        let mut content = vec![0u8; 4]; // version + flags
        write_u32(&mut content, 0); // sample_size = 0 (variable)
        write_u32(&mut content, sizes.len() as u32);
        for s in sizes {
            write_u32(&mut content, *s);
        }
        make_atom(b"stsz", &content)
    }

    fn make_stco(offsets: &[u32]) -> Vec<u8> {
        let mut content = vec![0u8; 4];
        write_u32(&mut content, offsets.len() as u32);
        for o in offsets {
            write_u32(&mut content, *o);
        }
        make_atom(b"stco", &content)
    }

    fn make_stss(sync_samples: &[u32]) -> Vec<u8> {
        let mut content = vec![0u8; 4];
        write_u32(&mut content, sync_samples.len() as u32);
        for s in sync_samples {
            write_u32(&mut content, *s);
        }
        make_atom(b"stss", &content)
    }

    fn make_minimal_mp4() -> Vec<u8> {
        // Build stbl
        let stsd = make_stsd_video();
        let stts = make_stts(&[(10, 1000)]); // 10 samples, 1000 ticks each
        let stsc = make_stsc(&[(1, 5, 1)]); // 5 samples per chunk starting at chunk 1
        let stsz = make_stsz(&[100, 200, 150, 100, 250, 100, 200, 150, 100, 250]);
        let stco = make_stco(&[1000, 2000]); // 2 chunks
        let stss = make_stss(&[1, 6]); // Keyframes at sample 1 and 6 (1-based)

        let mut stbl_content = Vec::new();
        stbl_content.extend_from_slice(&stsd);
        stbl_content.extend_from_slice(&stts);
        stbl_content.extend_from_slice(&stsc);
        stbl_content.extend_from_slice(&stsz);
        stbl_content.extend_from_slice(&stco);
        stbl_content.extend_from_slice(&stss);
        let stbl = make_atom(b"stbl", &stbl_content);

        let minf = make_atom(b"minf", &stbl);
        let hdlr = make_hdlr(b"vide");
        let mdhd = make_mdhd_v0(10000, 10000); // 10000 ticks / 10000 timescale = 1 sec

        let mut mdia_content = Vec::new();
        mdia_content.extend_from_slice(&mdhd);
        mdia_content.extend_from_slice(&hdlr);
        mdia_content.extend_from_slice(&minf);
        let mdia = make_atom(b"mdia", &mdia_content);

        let tkhd = make_tkhd_v0(1, 1920, 1080);

        let mut trak_content = Vec::new();
        trak_content.extend_from_slice(&tkhd);
        trak_content.extend_from_slice(&mdia);
        let trak = make_atom(b"trak", &trak_content);

        let mvhd = make_mvhd_v0(10000, 10000);

        let mut moov_content = Vec::new();
        moov_content.extend_from_slice(&mvhd);
        moov_content.extend_from_slice(&trak);
        let moov = make_atom(b"moov", &moov_content);

        // ftyp + mdat (empty) + moov
        let ftyp = make_atom(b"ftyp", b"isom\x00\x00\x00\x00isomiso2mp41");
        let mdat = make_atom(b"mdat", &vec![0u8; 2000]); // dummy data

        let mut mp4 = Vec::new();
        mp4.extend_from_slice(&ftyp);
        mp4.extend_from_slice(&mdat);
        mp4.extend_from_slice(&moov);
        mp4
    }

    #[test]
    fn test_parse_minimal_mp4() {
        let mp4 = make_minimal_mp4();
        let meta = parse_mp4(&mp4).unwrap();
        assert_eq!(meta.timescale, 10000);
        assert!((meta.duration_secs - 1.0).abs() < 0.01);
        assert_eq!(meta.tracks.len(), 1);

        let track = &meta.tracks[0];
        assert_eq!(track.track_id, 1);
        assert_eq!(track.track_type, TrackType::Video);
        assert_eq!(track.codec, "avc1");
        assert_eq!(track.timescale, 10000);
        assert_eq!(track.width, Some(1920));
        assert_eq!(track.height, Some(1080));
        assert_eq!(track.sample_table.stsz.len(), 10);
        assert_eq!(track.sample_table.stco.len(), 2);
        assert_eq!(track.sample_table.stss, Some(vec![1, 6]));
    }

    #[test]
    fn test_no_moov() {
        let ftyp = make_atom(b"ftyp", b"isom");
        let mdat = make_atom(b"mdat", &[0u8; 100]);
        let mut mp4 = Vec::new();
        mp4.extend_from_slice(&ftyp);
        mp4.extend_from_slice(&mdat);
        assert!(matches!(parse_mp4(&mp4), Err(Mp4Error::MoovNotFound)));
    }

    #[test]
    fn test_empty_data() {
        assert!(matches!(parse_mp4(&[]), Err(Mp4Error::MoovNotFound)));
    }

    #[test]
    fn test_sample_to_dts() {
        let stts = vec![
            SttsEntry {
                sample_count: 5,
                sample_delta: 1000,
            },
            SttsEntry {
                sample_count: 5,
                sample_delta: 2000,
            },
        ];
        assert_eq!(sample_to_dts(&stts, 0), 0);
        assert_eq!(sample_to_dts(&stts, 1), 1000);
        assert_eq!(sample_to_dts(&stts, 5), 5000);
        assert_eq!(sample_to_dts(&stts, 6), 7000);
        assert_eq!(sample_to_dts(&stts, 9), 13000);
    }

    #[test]
    fn test_dts_to_sample() {
        let stts = vec![
            SttsEntry {
                sample_count: 5,
                sample_delta: 1000,
            },
            SttsEntry {
                sample_count: 5,
                sample_delta: 2000,
            },
        ];
        assert_eq!(dts_to_sample(&stts, 0), 0);
        assert_eq!(dts_to_sample(&stts, 999), 0);
        assert_eq!(dts_to_sample(&stts, 1000), 1);
        assert_eq!(dts_to_sample(&stts, 5000), 5);
        assert_eq!(dts_to_sample(&stts, 7000), 6);
    }

    #[test]
    fn test_nearest_sync_sample() {
        let stss = Some(vec![1, 6, 11]); // 1-based
        assert_eq!(nearest_sync_sample(&stss, 0), 0); // sample 0 -> sync 1 (0-based: 0)
        assert_eq!(nearest_sync_sample(&stss, 3), 0); // sample 3 -> nearest sync before is 1 (0-based: 0)
        assert_eq!(nearest_sync_sample(&stss, 5), 5); // sample 5 -> sync 6 (0-based: 5)
        assert_eq!(nearest_sync_sample(&stss, 7), 5); // sample 7 -> nearest sync before is 6 (0-based: 5)
    }

    #[test]
    fn test_nearest_sync_sample_all_sync() {
        assert_eq!(nearest_sync_sample(&None, 5), 5);
        assert_eq!(nearest_sync_sample(&None, 0), 0);
    }

    #[test]
    fn test_total_samples() {
        let stts = vec![
            SttsEntry {
                sample_count: 5,
                sample_delta: 1000,
            },
            SttsEntry {
                sample_count: 3,
                sample_delta: 2000,
            },
        ];
        assert_eq!(total_samples(&stts), 8);
    }

    #[test]
    fn test_get_sample_offset() {
        let table = SampleTable {
            stts: vec![SttsEntry {
                sample_count: 10,
                sample_delta: 1000,
            }],
            stsc: vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 5,
                sample_description_index: 1,
            }],
            stsz: vec![100, 200, 150, 100, 250, 100, 200, 150, 100, 250],
            stco: vec![1000, 2000],
            stss: None,
            ctts: None,
        };

        // Sample 0: chunk 0, offset 1000, size 100
        let (off, sz) = get_sample_offset(&table, 0).unwrap();
        assert_eq!(off, 1000);
        assert_eq!(sz, 100);

        // Sample 1: chunk 0, offset 1000 + 100 = 1100, size 200
        let (off, sz) = get_sample_offset(&table, 1).unwrap();
        assert_eq!(off, 1100);
        assert_eq!(sz, 200);

        // Sample 5: chunk 1, offset 2000, size 100
        let (off, sz) = get_sample_offset(&table, 5).unwrap();
        assert_eq!(off, 2000);
        assert_eq!(sz, 100);
    }

    #[test]
    fn test_fixed_sample_size() {
        let mut content = vec![0u8; 4]; // version + flags
        write_u32(&mut content, 512); // fixed sample_size
        write_u32(&mut content, 5); // sample_count
        let atom_data = make_atom(b"stsz", &content);

        // Parse it manually
        let stbl_data = atom_data;
        let _stbl = Atom {
            fourcc: *b"stbl",
            header_size: 0,
            data_start: 0,
            data_end: stbl_data.len(),
        };
        // We need to wrap in stbl for find_atom to work
        let stbl_wrapped = make_atom(b"stbl", &stbl_data);
        let stbl_atom = Atom {
            fourcc: *b"stbl",
            header_size: 8,
            data_start: 8,
            data_end: stbl_wrapped.len(),
        };
        let sizes = parse_stsz(&stbl_wrapped, &stbl_atom).unwrap();
        assert_eq!(sizes, vec![512, 512, 512, 512, 512]);
    }

    #[test]
    fn test_co64_offsets() {
        let mut content = vec![0u8; 4]; // version + flags
        write_u32(&mut content, 2); // entry_count
        write_u64(&mut content, 0x1_0000_0000); // > 4GB offset
        write_u64(&mut content, 0x2_0000_0000);
        let co64_atom = make_atom(b"co64", &content);

        let stbl_wrapped = make_atom(b"stbl", &co64_atom);
        let stbl_atom = Atom {
            fourcc: *b"stbl",
            header_size: 8,
            data_start: 8,
            data_end: stbl_wrapped.len(),
        };
        let offsets = parse_stco(&stbl_wrapped, &stbl_atom).unwrap();
        assert_eq!(offsets, vec![0x1_0000_0000, 0x2_0000_0000]);
    }
}
