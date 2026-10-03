//! Reader for the EBK e-book container: the reader part of https://github.com/Sraw/ebk, for Readest.
//!
//! An EBK file holds the members of an EPUB under their original paths. Text members are
//! concatenated into a stream that is cut into independently compressed blocks; other members
//! are stored one by one. Every member read back is checked against its length and CRC-32.
//!
//! Feature `jpeg` (on by default): storage mode 4, JPEG files recompressed with Lepton. A reader built without it reports such members as unsupported and does not conform to the
//! specification, which asks readers for all five storage modes.

mod codepage;
mod error;
mod format;
mod index;
#[cfg(feature = "jpeg")]
mod jpeg;
mod reader;

pub use error::{Error, Result};
pub use index::{Member, Mode};
pub use reader::{Reader, Source};
