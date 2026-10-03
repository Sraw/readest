//! The index: block table, optional character table, member table (spec section 5).

use std::cmp::Ordering;

use crate::codepage::Table;
use crate::error::{invalid, Result};
use crate::format::*;

/// How a member's bytes are stored (spec section 5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// In the text stream, as is.
    Stream,
    /// In the text stream, recoded with the book's code page.
    StreamCoded,
    /// A resource item, as is.
    Raw,
    /// A resource item, one brotli stream.
    Brotli,
    /// A resource item, a JPEG recompressed reversibly.
    Jpeg,
    /// A storage mode defined by a later minor version; always a resource item.
    Unknown(u8),
}

impl Mode {
    pub fn from_u8(v: u8) -> Mode {
        match v {
            0 => Mode::Stream,
            1 => Mode::StreamCoded,
            2 => Mode::Raw,
            3 => Mode::Brotli,
            4 => Mode::Jpeg,
            v => Mode::Unknown(v),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Mode::Stream => 0,
            Mode::StreamCoded => 1,
            Mode::Raw => 2,
            Mode::Brotli => 3,
            Mode::Jpeg => 4,
            Mode::Unknown(v) => v,
        }
    }

    pub fn in_stream(self) -> bool {
        matches!(self, Mode::Stream | Mode::StreamCoded)
    }
}

/// One entry of the member table.
#[derive(Clone, Copy, Debug)]
pub struct Member<'a> {
    pub path: &'a str,
    pub mode: Mode,
    /// Length of the member's original bytes.
    pub raw_len: u64,
    /// Bytes the member occupies in the text stream or in the resource area.
    pub stored_len: u64,
    /// CRC-32 of the original bytes.
    pub crc32: u32,
}

/// One text block: a piece of the text stream compressed on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub raw_len: u64,
    pub packed_len: u64,
}

#[derive(Debug)]
pub(crate) enum Charset {
    Layout1(Table),
    /// A layout from a later minor version: the file opens, members that use the code page do not.
    Unknown(u8),
}

/// A member as the reader keeps it: 40 bytes, the path in a buffer shared by all members.
#[derive(Debug)]
pub(crate) struct Entry {
    pub raw_len: u64,
    pub stored_len: u64,
    /// Offset in the text stream, or file offset of the resource item; filled in by the reader.
    pub offset: u64,
    pub crc32: u32,
    path_at: u32,
    path_len: u16,
    pub mode: Mode,
}

#[derive(Debug)]
pub(crate) struct Index {
    pub blocks: Vec<Block>,
    pub charset: Option<Charset>,
    pub entries: Vec<Entry>,
    /// The paths of all members, one after the other.
    paths: String,
    /// Member numbers sorted by path (see `path_order`), for lookups.
    sorted: Vec<u32>,
}

/// Whether `path` can be the path of a member by itself (spec section 5.4): its length, its characters, its segments.
pub fn check_path(path: &str) -> std::result::Result<(), &'static str> {
    if path.is_empty() || path.len() as u64 > MAX_PATH_LEN {
        return Err("path length is out of range");
    }
    if path.chars().any(|c| c < '\u{20}' || c == '\u{7f}' || c == '\\') {
        return Err("path contains a control character or a backslash");
    }
    if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return Err("path has an empty, '.' or '..' segment");
    }
    Ok(())
}

/// Byte order with '/' before every other byte, so that a path is directly followed by the paths under it.
/// Paths that passed `check_path` contain no byte below 0x20, so two different paths never compare equal.
fn path_order(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // the common prefix is skipped a slice at a time: paths can share a thousand bytes
    let same = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let key = |s: &[u8]| s.get(same).map(|&c| if c == b'/' { 0 } else { c });
    key(a).cmp(&key(b))
}

/// Sorts `count` paths for lookups, and checks that none appears twice and that none is a directory
/// of another ("a" next to "a/b"). The paths must have passed `check_path`.
pub(crate) fn sort_paths<'a>(count: usize, path: impl Fn(usize) -> &'a str) -> Result<Vec<u32>> {
    let mut sorted = Vec::new();
    sorted.try_reserve_exact(count).map_err(|_| no_memory())?;
    sorted.extend(0..count as u32); // at most MAX_MEMBERS
    sorted.sort_unstable_by(|&a, &b| path_order(path(a as usize), path(b as usize)));
    for pair in sorted.windows(2) {
        let (a, b) = (path(pair[0] as usize), path(pair[1] as usize));
        if a == b {
            return invalid(format!("path appears twice: {a:?}"));
        }
        if b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/')) {
            return invalid(format!("{a:?} is both a member and a directory"));
        }
    }
    Ok(sorted)
}

impl Index {
    pub fn decode(buf: &[u8]) -> Result<Index> {
        let mut cur = Cursor::new(buf);
        let (mut blocks, mut charset, mut members) = (None, None, None);
        let mut last_tag = None;
        while cur.remaining() > 0 {
            let tag = cur.u8()?;
            if last_tag.is_some_and(|last| tag <= last) {
                return invalid("index sections are out of order or repeated");
            }
            last_tag = Some(tag);
            let len = cur.varint()?;
            let mut body = Cursor::new(cur.bytes(len)?);
            match tag {
                SECTION_BLOCKS => blocks = Some(decode_blocks(&mut body)?),
                SECTION_CHARSET => charset = Some(decode_charset(&mut body)?),
                SECTION_MEMBERS => members = Some(decode_members(&mut body)?),
                0x80.. => continue,
                _ => return invalid(format!("unknown required index section {tag:#04x}")),
            }
            if body.remaining() != 0 {
                return invalid(format!("index section {tag:#04x} has unused bytes"));
            }
        }
        let Some(blocks) = blocks else { return invalid("no block table") };
        let Some((entries, paths)) = members else { return invalid("no member table") };
        if entries.iter().any(|m| m.mode == Mode::StreamCoded) != charset.is_some() {
            return invalid("the character table must be present exactly when a member uses the code page");
        }
        let mut index = Index { blocks, charset, entries, paths, sorted: Vec::new() };
        index.sorted = sort_paths(index.entries.len(), |i| index.path(i))?;
        Ok(index)
    }

    pub fn path(&self, i: usize) -> &str {
        let e = &self.entries[i];
        &self.paths[e.path_at as usize..e.path_at as usize + e.path_len as usize]
    }

    /// Whether this version of the crate can read member `i`.
    pub fn readable(&self, i: usize) -> bool {
        match self.entries[i].mode {
            Mode::Stream | Mode::Raw | Mode::Brotli => true,
            Mode::StreamCoded => matches!(self.charset, Some(Charset::Layout1(_))),
            Mode::Jpeg => cfg!(feature = "jpeg"),
            Mode::Unknown(_) => false,
        }
    }

    pub fn member(&self, i: usize) -> Member<'_> {
        let e = &self.entries[i];
        Member { path: self.path(i), mode: e.mode, raw_len: e.raw_len, stored_len: e.stored_len, crc32: e.crc32 }
    }

    /// The number of the member with exactly this path.
    pub fn find(&self, path: &str) -> Option<usize> {
        let at = self.sorted.binary_search_by(|&i| path_order(self.path(i as usize), path)).ok()?;
        let i = self.sorted[at] as usize;
        // `path` is anything a caller passes: one with control characters can compare equal without being equal
        (self.path(i) == path).then_some(i)
    }
}

fn decode_blocks(cur: &mut Cursor) -> Result<Vec<Block>> {
    const MIN_ENTRY_LEN: u64 = 2;
    let count = cur.varint()?;
    if count > MAX_BLOCKS || count > cur.remaining() as u64 / MIN_ENTRY_LEN {
        return invalid("block count is out of range");
    }
    let mut blocks = Vec::new();
    blocks.try_reserve_exact(count as usize).map_err(|_| no_memory())?;
    for n in 0..count {
        let (raw_len, packed_len) = (cur.varint()?, cur.varint()?);
        let min = if n + 1 < count { MIN_BLOCK_LEN } else { 1 };
        if !(min..=MAX_BLOCK_SIZE).contains(&raw_len) {
            return invalid("length of a text block is out of range");
        }
        if !(1..=max_packed_len(raw_len)).contains(&packed_len) {
            return invalid("compressed length of a text block is out of range");
        }
        blocks.push(Block { raw_len, packed_len });
    }
    Ok(blocks)
}

fn decode_charset(cur: &mut Cursor) -> Result<Charset> {
    let layout = cur.u8()?;
    let table = cur.bytes(cur.remaining() as u64)?;
    if layout != 1 {
        return Ok(Charset::Unknown(layout));
    }
    // checked before anything is decoded: the section can be as long as the index
    if table.len() > 4 * MAX_CHARSET {
        return invalid("character table is too long");
    }
    let Ok(text) = std::str::from_utf8(table) else { return invalid("character table is not UTF-8") };
    let mut chars = Vec::new();
    chars.try_reserve_exact(MAX_CHARSET).map_err(|_| no_memory())?;
    for c in text.chars() {
        if chars.len() == MAX_CHARSET {
            return invalid("character table is too long");
        }
        chars.push(c);
    }
    let mut sorted = chars.clone();
    sorted.sort_unstable();
    if chars.iter().any(|&c| c <= '\u{7f}') || sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return invalid("character table has an ASCII or repeated character");
    }
    Ok(Charset::Layout1(Table::new(chars, sorted)))
}

fn decode_members(cur: &mut Cursor) -> Result<(Vec<Entry>, String)> {
    const MIN_ENTRY_LEN: u64 = 9;
    let count = cur.varint()?;
    if count > MAX_MEMBERS || count > cur.remaining() as u64 / MIN_ENTRY_LEN {
        return invalid("member count is out of range");
    }
    let mut entries = Vec::new();
    entries.try_reserve_exact(count as usize).map_err(|_| no_memory())?;
    let mut paths = String::new();
    for _ in 0..count {
        let mode = Mode::from_u8(cur.u8()?);
        let path_len = cur.varint()?;
        if path_len == 0 || path_len > MAX_PATH_LEN {
            return invalid("path length is out of range");
        }
        let Ok(path) = std::str::from_utf8(cur.bytes(path_len)?) else { return invalid("path is not UTF-8") };
        if let Err(why) = check_path(path) {
            return invalid(format!("{why}: {path:?}"));
        }
        let (raw_len, stored_len, crc32) = (cur.varint()?, cur.varint()?, cur.u32()?);
        let lengths_ok = match mode {
            Mode::Stream | Mode::Raw => stored_len == raw_len && (raw_len > 0 || crc32 == 0),
            // an empty member may not use the code page, so both lengths are at least 1
            Mode::StreamCoded => raw_len >= 1 && raw_len <= stored_len.saturating_mul(4) && stored_len <= raw_len.saturating_mul(2),
            Mode::Brotli => stored_len >= 1 && stored_len < raw_len,
            Mode::Jpeg => stored_len >= 1 && stored_len < raw_len && raw_len <= MAX_JPEG_LEN && raw_len <= stored_len.saturating_mul(MAX_JPEG_RATIO),
            // a later version defines what the lengths of its modes may be
            Mode::Unknown(_) => true,
        };
        if !lengths_ok {
            return invalid(format!("lengths of {path:?} do not fit its storage mode"));
        }
        // the paths together are shorter than the index, which is at most 64 MiB
        let (path_at, path_len) = (paths.len() as u32, path.len() as u16);
        paths.try_reserve(path.len()).map_err(|_| no_memory())?;
        paths.push_str(path);
        entries.push(Entry { raw_len, stored_len, offset: 0, crc32, path_at, path_len, mode });
    }
    Ok((entries, paths))
}
