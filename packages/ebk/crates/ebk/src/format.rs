//! Constants and primitive encodings of the format.

use crate::error::{invalid, Error, Result};

pub const MAGIC: [u8; 8] = [0x89, b'E', b'B', b'K', 0x0D, 0x0A, 0x1A, 0x0A];
pub const END_MAGIC: [u8; 4] = [b'E', b'B', b'K', 0x1A];
/// (major, minor) written by this crate. It reads every minor version of this major version.
pub const VERSION: (u8, u8) = (1, 0);
pub const HEADER_LEN: u64 = 16;
pub const FOOTER_LEN: u64 = 16;

/// Largest text block before compression.
pub const MAX_BLOCK_SIZE: u64 = 1 << 24;
/// Smallest text block before compression, the last block of a file excepted.
pub const MIN_BLOCK_LEN: u64 = 4096;
pub const MAX_BLOCKS: u64 = 1 << 20;
pub const MAX_INDEX_LEN: u64 = 64 << 20;
pub const MAX_MEMBERS: u64 = 1 << 20;
pub const MAX_PATH_LEN: u64 = 1024;
pub const MAX_CHARSET: usize = 8128;
/// Largest member a reader produces unless it is told otherwise.
pub const MAX_MEMBER_LEN: u64 = 1 << 30;
/// Longest JPEG file that can be stored recompressed (storage mode 4).
pub const MAX_JPEG_LEN: u64 = 1 << 27;
/// A recompressed JPEG is at most this many times longer than what is stored for it. Decoding costs in
/// proportion to the JPEG's length, so this ties the cost to bytes that are in the file.
pub const MAX_JPEG_RATIO: u64 = 8;
/// Largest image, in pixels, that a reader restores from storage mode 4 unless told otherwise; a
/// writer keeps larger ones as they are. Decoding takes up to 6 bytes of memory per pixel.
pub const DEFAULT_PIXEL_LIMIT: u64 = 1 << 26;

pub const SECTION_BLOCKS: u8 = 0x01;
pub const SECTION_CHARSET: u8 = 0x02;
pub const SECTION_MEMBERS: u8 = 0x03;

/// Largest compressed length allowed for a text block or an index of `raw_len` bytes (spec section 2).
pub fn max_packed_len(raw_len: u64) -> u64 {
    raw_len + raw_len / 4096 + 64
}

pub struct Cursor<'a> {
    buf: &'a [u8],
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Cursor { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn bytes(&mut self, n: u64) -> Result<&'a [u8]> {
        if n > self.buf.len() as u64 {
            return invalid("index ends in the middle of a field");
        }
        let (head, tail) = self.buf.split_at(n as usize);
        self.buf = tail;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    /// Unsigned LEB128, shortest form only, at most 2^63 - 1.
    pub fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..9 {
            let b = self.u8()?;
            v |= u64::from(b & 0x7F) << (7 * i);
            if b < 0x80 {
                if b == 0 && i > 0 {
                    return invalid("varint is not in its shortest form");
                }
                return Ok(v);
            }
        }
        invalid("varint is too long")
    }
}

pub fn no_memory() -> Error {
    Error::TooLarge("not enough memory".into())
}

/// A zeroed buffer, or an error instead of an abort when the memory is not there.
pub fn zeroed(len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| no_memory())?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| no_memory())?;
    buf.resize(len, 0);
    Ok(buf)
}

/// Why `inflate_exact` failed.
pub enum InflateError {
    /// The memory for the output is not there; the data may be fine.
    Memory,
    /// The data is not one brotli stream of the declared length.
    Stream(&'static str),
}

/// Memory handed to the brotli decoder.
#[derive(Default)]
struct Cells<T>(Vec<T>);

impl<T> brotli_decompressor::SliceWrapper<T> for Cells<T> {
    fn slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> brotli_decompressor::SliceWrapperMut<T> for Cells<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

/// The decoder's allocator. The decoder's own (`StandardAlloc`) aborts the process when memory runs
/// out, which in WebAssembly is a trap; this one hands back an empty slice, which the decoder takes
/// as "no memory" and reports as a failure, and notes that it happened.
struct TryAlloc<'a> {
    failed: &'a std::cell::Cell<bool>,
    /// Allocations still allowed; tests use it to make each allocation fail in turn.
    budget: &'a std::cell::Cell<usize>,
}

impl<T: Clone + Default> brotli_decompressor::Allocator<T> for TryAlloc<'_> {
    type AllocatedMemory = Cells<T>;

    fn alloc_cell(&mut self, len: usize) -> Cells<T> {
        let mut cells = Vec::new();
        let allowed = self.budget.get().checked_sub(1).map(|left| self.budget.set(left)).is_some();
        if !allowed || cells.try_reserve_exact(len).is_err() {
            self.failed.set(true);
            return Cells(Vec::new());
        }
        cells.resize(len, T::default());
        Cells(cells)
    }

    fn free_cell(&mut self, _cells: Cells<T>) {}
}

/// Decodes one complete brotli stream that must use all of `input` and produce exactly `len` bytes.
/// Streams that declare a window above 2^24 are refused. The output buffer grows with the data
/// actually produced, never ahead of it and never beyond `len`.
pub fn inflate_exact(input: &[u8], len: u64) -> std::result::Result<Vec<u8>, InflateError> {
    inflate_with(input, len, usize::MAX)
}

fn inflate_with(input: &[u8], len: u64, allocations: usize) -> std::result::Result<Vec<u8>, InflateError> {
    use brotli_decompressor::{BrotliDecompressStream, BrotliResult, BrotliState};
    use InflateError::{Memory, Stream};
    let len = usize::try_from(len).map_err(|_| Memory)?;
    let (failed, budget) = (std::cell::Cell::new(false), std::cell::Cell::new(allocations));
    let alloc = || TryAlloc { failed: &failed, budget: &budget };
    let mut state = BrotliState::new_strict(alloc(), alloc(), alloc());
    let (mut avail_in, mut in_pos, mut total) = (input.len(), 0, 0);
    let mut out = Vec::new();
    loop {
        // the decoder does not look at the table it allocates when it is created
        if failed.get() {
            return Err(Memory);
        }
        let filled = out.len();
        let room = (len - filled).min(filled.max(1 << 16));
        out.try_reserve_exact(room).map_err(|_| Memory)?;
        out.resize(filled + room, 0);
        let (mut avail_out, mut out_pos) = (room, 0);
        let result = BrotliDecompressStream(&mut avail_in, &mut in_pos, input, &mut avail_out, &mut out_pos,
                                            &mut out[filled..], &mut total, &mut state);
        out.truncate(filled + out_pos);
        match result {
            _ if failed.get() => return Err(Memory),
            BrotliResult::ResultSuccess if avail_in != 0 => return Err(Stream("data after the end of the brotli stream")),
            BrotliResult::ResultSuccess if out.len() != len => return Err(Stream("brotli stream is shorter than declared")),
            BrotliResult::ResultSuccess => return Ok(out),
            BrotliResult::NeedsMoreOutput if out.len() == len => return Err(Stream("brotli stream is longer than declared")),
            BrotliResult::NeedsMoreOutput => {}
            BrotliResult::NeedsMoreInput => return Err(Stream("brotli stream is truncated")),
            BrotliResult::ResultFailure => return Err(Stream("brotli stream is damaged")),
        }
    }
}
