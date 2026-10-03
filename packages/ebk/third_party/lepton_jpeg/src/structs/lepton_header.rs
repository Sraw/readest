// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.
use std::cmp::min;
use std::io::{Cursor, ErrorKind, Read, Seek};

use byteorder::{LittleEndian, ReadBytesExt};
use default_boxed::DefaultBoxed;
use flate2::read::ZlibDecoder;

use crate::EnabledFeatures;
use crate::consts::*;
use crate::helpers::buffer_prefix_matches_marker;
use crate::jpeg::jpeg_header::{JpegHeader, ReconstructionInfo};
use crate::lepton_error::{AddContext, ExitCode, LeptonError, Result, err_exit_code};
use crate::structs::thread_handoff::ThreadHandoff;

pub const FIXED_HEADER_SIZE: usize = 28;

#[derive(Debug, DefaultBoxed)]
pub struct LeptonHeader {
    /// how far we have read into the raw header, since the header is divided
    /// into multiple chucks for each scan. For example, a progressive image
    /// would start with the jpeg image segments, followed by a SOS (start of scan)
    /// after which comes the encoded jpeg coefficients, and once thats over
    /// we get another header segment until the next SOS, etc
    pub raw_jpeg_header_read_index: usize,

    pub thread_handoff: Vec<ThreadHandoff>,

    pub jpeg_header: JpegHeader,

    pub rinfo: ReconstructionInfo,

    pub jpeg_file_size: u32,

    /// on decompression, uncompressed lepton header size. This is only
    /// saved by this encoder for historical reasons. It is not used by
    /// the decoder.
    pub uncompressed_lepton_header_size: Option<u32>,

    /// the git revision of the encoder that created this file (first 8 hex characters)
    pub git_revision_prefix: [u8; 4],

    /// writer version
    pub encoder_version: u8,
}

/// EBK: reads `len` bytes into a buffer that grows with what is read. `len` comes from the file;
/// upstream allocated it up front, up to 4 GiB for a header of a few bytes.
fn read_declared<R: Read>(reader: &mut R, len: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    while data.len() < len {
        // room for what is still to come, a step at a time, and an error rather than an abort when there is none
        let step = (len - data.len()).min(data.len().max(1 << 16));
        if data.try_reserve_exact(step).is_err() {
            return err_exit_code(ExitCode::OutOfMemory, "no memory for the Lepton header");
        }
        let before = data.len();
        reader.by_ref().take(step as u64).read_to_end(&mut data)?;
        if data.len() - before != step {
            return Err(std::io::Error::from(ErrorKind::UnexpectedEof).into());
        }
    }
    Ok(data)
}

/// EBK: what the inflated header may hold beyond parts of the JPEG file itself (thread handoffs,
/// restart counts, markers).
const HEADER_SLACK: u64 = 1 << 16;

impl LeptonHeader {
    /// For certain versions of the rust encoder, we didn't handle truncation and corruption correctly.
    /// The correct behavior is to truncate the JPEG generated data up to the file size minus the garbage data,
    /// then write out the garbage data.
    ///
    /// The incorrect behavior was to write out the JPEG data, append the garbage data, and then truncate.
    pub fn bad_truncation_version(&self) -> bool {
        self.encoder_version == 55
    }

    pub fn read_lepton_fixed_header(
        &mut self,
        header: &[u8; FIXED_HEADER_SIZE],
        enabled_features: &mut EnabledFeatures,
    ) -> Result<usize> {
        if header[0..2] != LEPTON_FILE_HEADER[0..2] {
            return err_exit_code(ExitCode::BadLeptonFile, "header doesn't match");
        }
        if header[2] != LEPTON_VERSION {
            return err_exit_code(
                ExitCode::VersionUnsupported,
                format!("incompatible file with version {0}", header[3]),
            );
        }
        if header[3] != LEPTON_HEADER_BASELINE_JPEG_TYPE[0]
            && header[3] != LEPTON_HEADER_PROGRESSIVE_JPEG_TYPE[0]
        {
            return err_exit_code(
                ExitCode::BadLeptonFile,
                format!("Unknown filetype in header {0}", header[4]),
            );
        }

        // header[4] is the number of streams/threads, but we don't care about that
        // header[5..8] is reserved

        // header[8..20] 12 bytes were the GIT revision, but for historical reasons we
        // also use this space to store the uncompressed lepton header size plus some
        // flags to detect the SIMD flavor that was used to encode, since
        // previously the encoder would generate different incompatible files depending on
        // whether SIMD or scalar was selected by the build options.
        if header[8] == 'M' as u8 && header[9] == 'S' as u8 {
            self.uncompressed_lepton_header_size =
                Some(u32::from_le_bytes(header[10..14].try_into().unwrap()));

            // read the flag bits to know how we should decode this file
            let flags = header[14];
            if (flags & 0x80) != 0 {
                enabled_features.use_16bit_dc_estimate = (flags & 0x01) != 0;
                enabled_features.use_16bit_adv_predict = (flags & 0x02) != 0;
            }

            self.encoder_version = header[15];
            self.git_revision_prefix = header[16..20].try_into().unwrap();
        } else {
            // take first bytes for git revision prefix
            self.git_revision_prefix = header[8..12].try_into().unwrap();
        }

        // total size of original JPEG
        self.jpeg_file_size = u32::from_le_bytes(header[20..24].try_into().unwrap());

        let compressed_header_size =
            u32::from_le_bytes(header[24..28].try_into().unwrap()) as usize;

        Ok(compressed_header_size)
    }

    /// reads the start of the lepton file and parses the compressed header. Returns the raw JPEG header contents.
    pub fn read_compressed_lepton_header<R: Read>(
        &mut self,
        reader: &mut R,
        enabled_features: &mut EnabledFeatures,
        compressed_header_size: usize,
    ) -> Result<()> {
        if compressed_header_size > enabled_features.max_jpeg_file_size as usize {
            return err_exit_code(ExitCode::BadLeptonFile, "Too big compressed header");
        }
        if self.jpeg_file_size > enabled_features.max_jpeg_file_size {
            return err_exit_code(
                ExitCode::BadLeptonFile,
                format!(
                    "Only support images < {} megs",
                    enabled_features.max_jpeg_file_size / (1024 * 1024)
                ),
            );
        }

        // limit reading to the compressed header
        let mut compressed_reader = reader.take(compressed_header_size as u64);

        self.rinfo.raw_jpeg_header = self
            .read_lepton_compressed_header(&mut compressed_reader)
            .context()?;

        self.raw_jpeg_header_read_index = 0;

        {
            let mut header_data_cursor = Cursor::new(&self.rinfo.raw_jpeg_header[..]);
            // EBK: `parse` returns false when the headers end without a scan. Upstream ignores that and
            // goes on with component sizes that were never computed (block width u32::MAX).
            if !self
                .jpeg_header
                .parse(&mut header_data_cursor, &enabled_features)
                .context()?
            {
                return err_exit_code(ExitCode::BadLeptonFile, "JPEG header ends before the first scan");
            }
            self.raw_jpeg_header_read_index = header_data_cursor.position() as usize;
        }

        // EBK: the encoder refuses a fourth component, and the decoder is written for three (upstream
        // asserts that when it works out the rows)
        if self.jpeg_header.cmpc > COLOR_CHANNEL_NUM_BLOCK_TYPES {
            return err_exit_code(ExitCode::Unsupported4Colors, "doesn't support 4 color channels");
        }

        self.rinfo.truncate_components.init(&self.jpeg_header);

        if self.rinfo.early_eof_encountered {
            self.rinfo
                .truncate_components
                .set_truncation_bounds(&self.jpeg_header, self.rinfo.max_dpos);
        }

        let num_threads = self.thread_handoff.len();
        // EBK: a header without any thread handoff has nothing to decode with (upstream indexes the list at -1)
        if num_threads == 0 {
            return err_exit_code(ExitCode::BadLeptonFile, "no thread handoff in the header");
        }
        // EBK: the limit of the format, which upstream checks when it writes only
        if num_threads > MAX_THREADS_SUPPORTED_BY_LEPTON_FORMAT {
            return err_exit_code(ExitCode::BadLeptonFile, "too many thread handoffs in the header");
        }

        // luma_y_end of the last thread is not serialized/deserialized, fill it here
        let max_luma = self.rinfo.truncate_components.get_block_height(0);

        for i in 0..num_threads {
            self.thread_handoff[i].luma_y_start =
                min(self.thread_handoff[i].luma_y_start, max_luma);
            self.thread_handoff[i].luma_y_end = min(self.thread_handoff[i].luma_y_end, max_luma);
        }
        self.thread_handoff[num_threads - 1].luma_y_end = max_luma;

        // EBK: each part ends where the next one starts, so starts that go backwards give a part that ends
        // before it starts (upstream subtracts the two when it sizes the part's image)
        if self.thread_handoff.iter().any(|t| t.luma_y_start > t.luma_y_end) {
            return err_exit_code(ExitCode::BadLeptonFile, "the parts of the image are out of order");
        }

        // if the last segment was too big to fit with the garbage data taken into account, shorten it
        // (a bit of broken logic in the encoder, but can't change it without breaking the file format)
        if self.rinfo.early_eof_encountered {
            // EBK: the parts are taken off the file size with checks; in a damaged file they add up to more than it
            let too_long = || LeptonError::new(ExitCode::BadLeptonFile, "the parts of the file are longer than the file");
            let mut max_last_segment_size = self
                .jpeg_file_size
                .checked_sub(u32::try_from(self.rinfo.garbage_data.len())?)
                .and_then(|left| left.checked_sub(u32::try_from(self.raw_jpeg_header_read_index).ok()?))
                .and_then(|left| left.checked_sub(SOI.len() as u32))
                .ok_or_else(too_long)?;

            // subtract the segment sizes of all the previous segments (except for the last)
            for i in 0..num_threads - 1 {
                max_last_segment_size = max_last_segment_size
                    .checked_sub(self.thread_handoff[i].segment_size)
                    .ok_or_else(too_long)?;
            }

            let last = &mut self.thread_handoff[num_threads - 1];

            let max_last_segment_size = max_last_segment_size;

            if last.segment_size > max_last_segment_size {
                // re-adjust the last segment size
                last.segment_size = max_last_segment_size;
            }
        }

        Ok(())
    }

    /// parses and advances to the next header segment out of raw_jpeg_header into the jpeg header
    pub fn advance_next_header_segment(
        &mut self,
        enabled_features: &EnabledFeatures,
    ) -> Result<bool> {
        let mut header_cursor =
            Cursor::new(&self.rinfo.raw_jpeg_header[self.raw_jpeg_header_read_index..]);

        let result = self
            .jpeg_header
            .parse(&mut header_cursor, enabled_features)
            .context()?;

        self.raw_jpeg_header_read_index += header_cursor.stream_position()? as usize;

        Ok(result)
    }

    /// helper for read_lepton_header. uncompresses and parses the contents of the compressed header. Returns the raw JPEG header.
    fn read_lepton_compressed_header<R: Read>(&mut self, src: &mut R) -> Result<Vec<u8>> {
        // EBK: the header holds parts of the JPEG file (its own headers, restart data, bytes after the
        // image), so inflated it is no longer than that file plus a little. Upstream has no bound: zlib
        // expands a thousandfold, and records may repeat, so 800 KB of header became 1.6 GB of memory.
        let limit = u64::from(self.jpeg_file_size) + HEADER_SLACK;
        let mut header_reader = ZlibDecoder::new(src).take(limit + 1);

        let mut hdr_buf: [u8; 3] = [0; 3];
        header_reader.read_exact(&mut hdr_buf)?;

        if !buffer_prefix_matches_marker(hdr_buf, LEPTON_HEADER_MARKER) {
            return err_exit_code(ExitCode::BadLeptonFile, "HDR marker not found");
        }

        let hdrs = header_reader.read_u32::<LittleEndian>()? as usize;

        let hdr_data = read_declared(&mut header_reader, hdrs)?;

        if self.rinfo.garbage_data.len() == 0 {
            // if we don't have any garbage, assume 0xFF 0xD9 EOI (end of image marker)

            // Kind of broken logic since this assumes a EOI even if the file was
            // truncated at the EOI, but this is what the file format is.
            // In this case, this marker will be chopped off later by the
            // overall JPEG file size limit, so this is not a correctness problem.
            self.rinfo.garbage_data.extend(EOI);
        }

        // beginning here: recovery information (needed for exact JPEG recovery)
        // read further recovery information if any
        loop {
            let mut current_lepton_marker = [0u8; 3];
            match header_reader.read_exact(&mut current_lepton_marker) {
                Ok(_) => {}
                Err(e) => {
                    if e.kind() == ErrorKind::UnexpectedEof {
                        break;
                    } else {
                        return Err(e.into());
                    }
                }
            }

            if buffer_prefix_matches_marker(current_lepton_marker, LEPTON_HEADER_PAD_MARKER) {
                self.rinfo.pad_bit = Some(header_reader.read_u8()?);
            } else if buffer_prefix_matches_marker(
                current_lepton_marker,
                LEPTON_HEADER_JPG_RESTARTS_MARKER,
            ) {
                // CRS marker
                self.rinfo.rst_cnt_set = true;
                let rst_count = header_reader.read_u32::<LittleEndian>()?;

                for _i in 0..rst_count {
                    self.rinfo
                        .rst_cnt
                        .push(header_reader.read_u32::<LittleEndian>()?);
                }
            } else if buffer_prefix_matches_marker(
                current_lepton_marker,
                LEPTON_HEADER_LUMA_SPLIT_MARKER,
            ) {
                // HH markup
                // EBK: one list, as the encoder writes it. Upstream appends every list it finds; each part
                // of each list gets its own image, so a damaged file could ask for the image many times over
                if !self.thread_handoff.is_empty() {
                    return err_exit_code(ExitCode::BadLeptonFile, "more than one list of thread handoffs");
                }
                let mut thread_handoffs =
                    ThreadHandoff::deserialize(current_lepton_marker[2], &mut header_reader)?;

                self.thread_handoff.append(&mut thread_handoffs);
            } else if buffer_prefix_matches_marker(
                current_lepton_marker,
                LEPTON_HEADER_JPG_RESTART_ERRORS_MARKER,
            ) {
                // Marker FRS
                // read number of false set RST markers per scan from file
                let rst_err_count = header_reader.read_u32::<LittleEndian>()? as usize;

                let mut rst_err_data = read_declared(&mut header_reader, rst_err_count)?;

                self.rinfo.rst_err.append(&mut rst_err_data);
            } else if buffer_prefix_matches_marker(
                current_lepton_marker,
                LEPTON_HEADER_GARBAGE_MARKER,
            ) {
                // GRB marker
                // read garbage (data after end of JPG) from file
                let garbage_size = header_reader.read_u32::<LittleEndian>()? as usize;

                self.rinfo.garbage_data = read_declared(&mut header_reader, garbage_size)?;
            } else if buffer_prefix_matches_marker(
                current_lepton_marker,
                LEPTON_HEADER_EARLY_EOF_MARKER,
            ) {
                self.rinfo.max_cmp = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.max_bpos = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.max_sah = u8::try_from(header_reader.read_u32::<LittleEndian>()?)?;
                self.rinfo.max_dpos[0] = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.max_dpos[1] = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.max_dpos[2] = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.max_dpos[3] = header_reader.read_u32::<LittleEndian>()?;
                self.rinfo.early_eof_encountered = true;
            } else {
                return err_exit_code(ExitCode::BadLeptonFile, "unknown data found");
            }
        }

        // EBK: reading stopped because the bound was reached, not because the header ended
        if header_reader.limit() == 0 {
            return err_exit_code(ExitCode::BadLeptonFile, "header is larger than the JPEG file it describes");
        }

        return Ok(hdr_data);
    }
}

// test serializing and deserializing header
