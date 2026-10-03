//! The per-book code page, layout 1 (spec section 6): the 64 most frequent non-ASCII characters of
//! a book take one byte, the next 8064 two bytes, any other four.

use crate::format::MAX_CHARSET;

/// Characters with a one-byte code.
const SHORT: usize = 64;
const ESCAPE: u8 = 0xFF;

/// The characters of a book's code page; the position of a character is its rank.
#[derive(Debug)]
pub(crate) struct Table {
    chars: Vec<char>,
    /// The same characters in code point order, to tell whether a character has a rank.
    sorted: Vec<char>,
}

pub(crate) enum DecodeError {
    Memory,
    Data(&'static str),
}

impl Table {
    /// `chars` holds no ASCII character, no character twice, and at most `MAX_CHARSET` characters.
    pub fn new(chars: Vec<char>, sorted: Vec<char>) -> Table {
        debug_assert!(chars.len() <= MAX_CHARSET && chars.len() == sorted.len());
        Table { chars, sorted }
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Turns the stored bytes of a member back into its UTF-8 text, which must be `raw_len` bytes long.
    pub fn decode(&self, stored: &[u8], raw_len: u64) -> Result<Vec<u8>, DecodeError> {
        const CUT: DecodeError = DecodeError::Data("coded text ends inside a character");
        const TRAIL: DecodeError = DecodeError::Data("coded text has a second byte below 0x80");
        const LENGTH: DecodeError = DecodeError::Data("decoded text does not have the declared length");
        // the index was checked for raw_len <= 4 * stored length, so this is in proportion to data that exists
        let raw_len = usize::try_from(raw_len).map_err(|_| DecodeError::Memory)?;
        let mut out = Vec::new();
        out.try_reserve_exact(raw_len).map_err(|_| DecodeError::Memory)?;
        let mut rest = stored;
        while let Some((&b, tail)) = rest.split_first() {
            // text is mostly runs of ASCII (markup) and runs of coded characters
            if b < 0x80 {
                let run = rest.iter().position(|&b| b >= 0x80).unwrap_or(rest.len());
                if run > raw_len - out.len() {
                    return Err(LENGTH);
                }
                out.extend_from_slice(&rest[..run]);
                rest = &rest[run..];
                continue;
            }
            let c = if b < 0xC0 {
                rest = tail;
                self.chars.get(usize::from(b - 0x80)).copied()
            } else if b < ESCAPE {
                let Some((&t, tail)) = tail.split_first() else { return Err(CUT) };
                if t < 0x80 {
                    return Err(TRAIL);
                }
                rest = tail;
                self.chars.get(SHORT + usize::from(b - 0xC0) * 128 + usize::from(t - 0x80)).copied()
            } else {
                let Some((&[x, y, z], tail)) = tail.split_first_chunk::<3>() else { return Err(CUT) };
                if x < 0x80 || y < 0x80 || z < 0x80 {
                    return Err(TRAIL);
                }
                rest = tail;
                let code = u32::from(x - 0x80) << 14 | u32::from(y - 0x80) << 7 | u32::from(z - 0x80);
                let Some(c) = char::from_u32(code).filter(|&c| c > '\u{7f}') else {
                    return Err(DecodeError::Data("coded text has a four-byte form that is not a non-ASCII character"));
                };
                if self.sorted.binary_search(&c).is_ok() {
                    return Err(DecodeError::Data("coded text spells out a character that has a code"));
                }
                Some(c)
            };
            let Some(c) = c else { return Err(DecodeError::Data("coded text uses a code beyond the character table")) };
            // never past the reserved length: the buffer does not grow
            if c.len_utf8() > raw_len - out.len() {
                return Err(LENGTH);
            }
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
        if out.len() != raw_len {
            return Err(LENGTH);
        }
        Ok(out)
    }
}
