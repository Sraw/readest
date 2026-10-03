//! Reading an EBK file: validate the header, footer and index once, then serve members on demand.

use crate::error::{invalid, Error, Result};
use crate::format::*;
use crate::codepage::DecodeError;
use crate::index::{Block, Charset, Index, Member, Mode};

/// Random access to the bytes of an EBK file, so that a reader need not hold the whole file.
pub trait Source {
    fn len(&self) -> u64;
    /// Fills `buf` with the bytes at `offset`.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

fn slice_read_at(data: &[u8], offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    let range = usize::try_from(offset).ok().and_then(|start| Some(start..start.checked_add(buf.len())?));
    match range.and_then(|r| data.get(r)) {
        Some(src) => Ok(buf.copy_from_slice(src)),
        None => Err(std::io::ErrorKind::UnexpectedEof.into()),
    }
}

impl Source for &[u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        slice_read_at(self, offset, buf)
    }
}

impl Source for Vec<u8> {
    fn len(&self) -> u64 {
        Vec::len(self) as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        slice_read_at(self, offset, buf)
    }
}

/// Decoded text blocks kept for the next read. Two, because a chapter may straddle a block boundary.
const CACHED_BLOCKS: usize = 2;

pub struct Reader<S> {
    src: S,
    version: (u8, u8),
    index: Index,
    index_len: u64,
    stream_len: u64,
    member_limit: u64,
    pixel_limit: u64,
    /// File offset of each text block, and of the end of the last one (where the resource area starts).
    block_offsets: Vec<u64>,
    /// Offset of each text block in the text stream, and the length of the stream.
    block_starts: Vec<u64>,
    cache: Vec<(usize, Vec<u8>)>,
    /// Blocks that failed to decode. They are not decoded again: many members can lie in one block.
    damaged: Bits,
    /// Blocks there was no memory for; not tried again until `retry_starved`.
    starved: Bits,
}

/// One bit per text block.
struct Bits(Vec<u8>);

impl Bits {
    fn new(len: usize) -> Result<Bits> {
        Ok(Bits(zeroed(len.div_ceil(8) as u64)?))
    }

    fn get(&self, i: usize) -> bool {
        self.0[i / 8] & 1 << (i % 8) != 0
    }

    fn set(&mut self, i: usize) {
        self.0[i / 8] |= 1 << (i % 8);
    }
}

impl<S: Source> Reader<S> {
    /// Validates the header, the footer and the whole index (spec section 3.3). No member is read.
    pub fn open(src: S) -> Result<Self> {
        let file_len = src.len();
        if file_len < HEADER_LEN + FOOTER_LEN {
            return invalid("file is too short");
        }
        let mut header = [0u8; HEADER_LEN as usize];
        src.read_at(0, &mut header)?;
        if header[..8] != MAGIC {
            return invalid("wrong signature");
        }
        // every minor version of the major version is accepted: what a later one adds can be skipped
        // (optional sections) or refused member by member (storage modes, character-table layouts)
        if header[8] != VERSION.0 {
            return invalid(format!("format version {}.{}, this reader knows {}.x", header[8], header[9], VERSION.0));
        }
        let version = (header[8], header[9]);
        if header[10..] != [0; 6] {
            return invalid("header flags or reserved bytes are set");
        }

        let mut footer = [0u8; FOOTER_LEN as usize];
        src.read_at(file_len - FOOTER_LEN, &mut footer)?;
        if footer[12..] != END_MAGIC {
            return invalid("wrong end signature (truncated file?)");
        }
        let word = |i: usize| u64::from(u32::from_le_bytes(footer[i..i + 4].try_into().unwrap()));
        let (index_len, index_raw_len, index_crc) = (word(0), word(4), word(8) as u32);
        if index_len > MAX_INDEX_LEN || index_raw_len > MAX_INDEX_LEN || index_len > file_len - HEADER_LEN - FOOTER_LEN
            || !(1..=max_packed_len(index_raw_len)).contains(&index_len)
        {
            return invalid("index length is out of range");
        }
        let mut packed = zeroed(index_len)?;
        src.read_at(file_len - FOOTER_LEN - index_len, &mut packed)?;
        let raw = inflate_exact(&packed, index_raw_len).map_err(|e| match e {
            InflateError::Memory => no_memory(),
            InflateError::Stream(why) => Error::Invalid(format!("index: {why}")),
        })?;
        drop(packed);
        if crc32fast::hash(&raw) != index_crc {
            return invalid("index checksum does not match");
        }
        let mut index = Index::decode(&raw)?;
        drop(raw);

        let overflow = || Error::Invalid("lengths in the index overflow".into());
        let (mut block_offsets, mut block_starts) = (Vec::new(), Vec::new());
        block_offsets.try_reserve_exact(index.blocks.len() + 1).map_err(|_| no_memory())?;
        block_starts.try_reserve_exact(index.blocks.len() + 1).map_err(|_| no_memory())?;
        let (damaged, starved) = (Bits::new(index.blocks.len())?, Bits::new(index.blocks.len())?);
        let (mut pos, mut blocks_raw_len) = (HEADER_LEN, 0u64);
        for block in &index.blocks {
            block_offsets.push(pos);
            block_starts.push(blocks_raw_len);
            pos = pos.checked_add(block.packed_len).ok_or_else(overflow)?;
            blocks_raw_len += block.raw_len; // at most 2^20 blocks of 2^24 bytes
        }
        block_offsets.push(pos);
        block_starts.push(blocks_raw_len);
        let mut stream_len = 0u64;
        for m in &mut index.entries {
            let at = if m.mode.in_stream() { &mut stream_len } else { &mut pos };
            m.offset = *at;
            *at = at.checked_add(m.stored_len).ok_or_else(overflow)?;
        }
        if stream_len != blocks_raw_len {
            return invalid("the text blocks do not add up to the length of the text stream");
        }
        if pos.checked_add(index_len + FOOTER_LEN) != Some(file_len) {
            return invalid("the index does not account for every byte of the file");
        }
        Ok(Reader { src, version, index, index_len, stream_len, member_limit: MAX_MEMBER_LEN, pixel_limit: DEFAULT_PIXEL_LIMIT, block_offsets, block_starts, cache: Vec::new(), damaged, starved })
    }

    /// The source the reader was opened on.
    pub fn source(&self) -> &S {
        &self.src
    }

    /// Major and minor version of the format the file says it has.
    pub fn version(&self) -> (u8, u8) {
        self.version
    }

    pub fn blocks(&self) -> &[Block] {
        &self.index.blocks
    }

    pub fn stream_len(&self) -> u64 {
        self.stream_len
    }

    /// Compressed length of the index.
    pub fn index_len(&self) -> u64 {
        self.index_len
    }

    pub fn member_count(&self) -> usize {
        self.index.entries.len()
    }

    /// Member number `i`, counting from 0 in the order of the member table.
    pub fn member(&self, i: usize) -> Option<Member<'_>> {
        (i < self.member_count()).then(|| self.index.member(i))
    }

    pub fn members(&self) -> impl ExactSizeIterator<Item = Member<'_>> {
        (0..self.member_count()).map(|i| self.index.member(i))
    }

    /// Whether this version of the crate can read member `i`: its storage mode is one it implements,
    /// and for a member that uses the code page, so is the layout of the character table.
    pub fn readable(&self, i: usize) -> bool {
        i < self.member_count() && self.index.readable(i)
    }

    /// The number of characters in the book's code page, if it has one of a layout this version knows.
    pub fn charset_len(&self) -> Option<usize> {
        match &self.index.charset {
            Some(Charset::Layout1(table)) => Some(table.len()),
            _ => None,
        }
    }

    /// Looks a member up by its exact path.
    pub fn find(&self, path: &str) -> Option<usize> {
        self.index.find(path)
    }

    /// A text block that could not be decoded for lack of memory fails at once from then on, since
    /// every member in it would otherwise try again. This makes such blocks be tried again.
    pub fn retry_starved(&mut self) {
        self.starved.0.fill(0);
    }

    /// Sets the largest member `read` will produce (default `MAX_MEMBER_LEN`).
    pub fn set_member_limit(&mut self, bytes: u64) {
        self.member_limit = bytes;
    }

    /// Sets the largest image, in pixels, that `read` restores from a recompressed JPEG (default
    /// `DEFAULT_PIXEL_LIMIT`). The decoder needs up to 6 bytes of memory per pixel.
    pub fn set_pixel_limit(&mut self, pixels: u64) {
        self.pixel_limit = pixels;
    }

    /// Returns the original bytes of member `i`, checked against its length and CRC-32.
    pub fn read(&mut self, i: usize) -> Result<Vec<u8>> {
        let Some(m) = self.member(i) else { return Err(Error::NoSuchMember(i)) };
        let (mode, raw_len, crc32) = (m.mode, m.raw_len, m.crc32);
        let (offset, stored_len) = (self.index.entries[i].offset, m.stored_len);
        // errors below do not know which member they belong to; the path is only formatted when one happens
        let named = |index: &Index, e: Error| {
            let path = index.path(i).escape_debug();
            match e {
                Error::Corrupt(why) => Error::Corrupt(format!("{path}: {why}")),
                Error::TooLarge(why) => Error::TooLarge(format!("{path}: {why}")),
                Error::Unsupported(why) => Error::Unsupported(format!("{path}: {why}")),
                e => e,
            }
        };
        // first: the lengths of a mode this version does not know mean nothing to it
        if !self.index.readable(i) {
            let what = match (mode, &self.index.charset) {
                (Mode::StreamCoded, Some(Charset::Unknown(layout))) => format!("character table layout {layout}"),
                _ => format!("storage mode {}", mode.to_u8()),
            };
            return Err(named(&self.index, Error::Unsupported(what)));
        }
        if raw_len > self.member_limit {
            return Err(named(&self.index, Error::TooLarge("larger than this reader allows".into())));
        }
        let data = match mode {
            Mode::Stream => self.read_stream(offset, stored_len),
            Mode::Raw => self.read_stored(offset, stored_len),
            Mode::Brotli => self.read_stored(offset, stored_len).and_then(|packed| {
                inflate_exact(&packed, raw_len).map_err(|e| match e {
                    InflateError::Memory => no_memory(),
                    InflateError::Stream(why) => Error::Corrupt(why.into()),
                })
            }),
            Mode::StreamCoded => self.read_stream(offset, stored_len).and_then(|coded| match &self.index.charset {
                Some(Charset::Layout1(table)) => table.decode(&coded, raw_len).map_err(|e| match e {
                    DecodeError::Memory => no_memory(),
                    DecodeError::Data(why) => Error::Corrupt(why.into()),
                }),
                _ => unreachable!("checked above"),
            }),
            #[cfg(feature = "jpeg")]
            Mode::Jpeg => self.read_stored(offset, stored_len).and_then(|stored| {
                crate::jpeg::restore(&stored, raw_len, self.pixel_limit).map_err(|e| match e {
                    crate::jpeg::JpegError::Memory => Error::TooLarge("the image needs more memory than there is or than this reader allows".into()),
                    crate::jpeg::JpegError::Data(why) => Error::Corrupt(why),
                })
            }),
            #[cfg(not(feature = "jpeg"))]
            Mode::Jpeg => unreachable!("checked above"),
            Mode::Unknown(_) => unreachable!("checked above"),
        };
        let data = data.map_err(|e| named(&self.index, e))?;
        if data.len() as u64 != raw_len {
            return Err(named(&self.index, Error::Corrupt("length does not match".into())));
        }
        if crc32fast::hash(&data) != crc32 {
            return Err(named(&self.index, Error::Corrupt("checksum does not match".into())));
        }
        Ok(data)
    }

    fn read_stored(&self, offset: u64, stored_len: u64) -> Result<Vec<u8>> {
        // open() checked that the item lies inside the file; for the modes read here it is no longer than the member
        let mut buf = zeroed(stored_len)?;
        self.src.read_at(offset, &mut buf)?;
        Ok(buf)
    }

    /// Bytes `offset..offset + len` of the text stream; open() checked that they lie inside it.
    fn read_stream(&mut self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let (mut pos, end) = (offset, offset + len);
        if pos == end {
            return Ok(out);
        }
        let len = usize::try_from(len).map_err(|_| no_memory())?;
        let mut b = self.block_starts.partition_point(|&start| start <= pos) - 1;
        while pos < end {
            let base = self.block_starts[b];
            let block = self.block(b)?;
            let stop = usize::try_from(end - base).map_or(block.len(), |stop| stop.min(block.len()));
            let piece = &block[(pos - base) as usize..stop];
            if out.capacity() - out.len() < piece.len() {
                // grows with the blocks that did decode, doubling, and never beyond the member
                let room = (len - out.len()).min(out.len().max(piece.len()));
                out.try_reserve_exact(room).map_err(|_| no_memory())?;
            }
            out.extend_from_slice(piece);
            pos = base + block.len() as u64;
            b += 1;
        }
        Ok(out)
    }

    fn block(&mut self, b: usize) -> Result<&[u8]> {
        if let Some(hit) = self.cache.iter().position(|(n, _)| *n == b) {
            let entry = self.cache.remove(hit);
            self.cache.push(entry);
        } else {
            if self.damaged.get(b) {
                return Err(Error::Corrupt(format!("text block {b} is damaged")));
            }
            if self.starved.get(b) {
                return Err(Error::TooLarge(format!("there was not enough memory for text block {b}")));
            }
            let block = self.index.blocks[b];
            let Ok(mut packed) = zeroed(block.packed_len) else {
                self.starved.set(b);
                return Err(no_memory());
            };
            self.src.read_at(self.block_offsets[b], &mut packed)?;
            let raw = inflate_exact(&packed, block.raw_len).map_err(|e| match e {
                InflateError::Memory => {
                    self.starved.set(b);
                    no_memory()
                }
                InflateError::Stream(why) => {
                    self.damaged.set(b);
                    Error::Corrupt(format!("text block {b}: {why}"))
                }
            })?;
            if self.cache.len() == CACHED_BLOCKS {
                self.cache.remove(0);
            }
            self.cache.push((b, raw));
        }
        Ok(&self.cache.last().unwrap().1)
    }
}
