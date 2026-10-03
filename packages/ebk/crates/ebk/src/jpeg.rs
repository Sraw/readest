//! Storage mode 4: a JPEG kept as a Lepton file, which is smaller and gives back the same bytes
//! (spec section 7). The codec is the copy of `lepton_jpeg` in `third_party/`.

use std::io::{Cursor, Write};

use lepton_jpeg::{decode_lepton, EnabledFeatures, ExitCode, SingleThreadPool};

use crate::format::MAX_JPEG_LEN;

pub(crate) enum JpegError {
    /// The image needs more memory than there is, or than the reader allows.
    Memory,
    Data(String),
}

/// The output of the decoder: never more than the declared length, growing with what arrives.
struct Exact {
    out: Vec<u8>,
    len: usize,
    no_memory: bool,
}

impl Write for Exact {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = self.len - self.out.len();
        if buf.len() > room {
            return Err(std::io::Error::other("more data than declared"));
        }
        if self.out.capacity() - self.out.len() < buf.len() {
            let more = room.min(self.out.len().max(buf.len()).max(1 << 16));
            if self.out.try_reserve_exact(more).is_err() {
                self.no_memory = true;
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs the codec. It is a large body of code that was not written to face hostile files: a panic
/// in it is reported as bad data where panics can be caught.
fn guarded<T>(run: impl FnOnce() -> Result<T, JpegError>) -> Result<T, JpegError> {
    #[cfg(panic = "unwind")]
    return std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| Err(JpegError::Data("the JPEG codec failed".into())));
    #[cfg(not(panic = "unwind"))]
    return run();
}

/// Turns the stored bytes of a member back into its JPEG file of `raw_len` bytes.
/// Images of more than `max_pixels` pixels are refused before anything is allocated for them.
pub(crate) fn restore(stored: &[u8], raw_len: u64, max_pixels: u64) -> Result<Vec<u8>, JpegError> {
    // the index was checked for this; the file header repeats the length
    let len = u32::try_from(raw_len).ok().filter(|_| raw_len <= MAX_JPEG_LEN).ok_or_else(|| JpegError::Data("longer than a recompressed JPEG can be".into()))?;
    if stored.get(20..24) != Some(&len.to_le_bytes()[..]) {
        return Err(JpegError::Data("not a Lepton file of the declared length".into()));
    }
    // The settings the writer encodes with, not the codec's lenient ones for reading: the header of a Lepton file holds
    // the headers of the JPEG, and one that the writer would have refused (Huffman tables the standard does not allow,
    // zeros in a quantisation table, a side over 16386 pixels) cannot be in a file a writer made.
    let features = EnabledFeatures { max_jpeg_file_size: len, max_jpeg_pixels: max_pixels, max_processor_threads: 1, ..EnabledFeatures::compat_lepton_vector_write() };
    guarded(|| {
        let mut exact = Exact { out: Vec::new(), len: len as usize, no_memory: false };
        match decode_lepton(&mut Cursor::new(stored), &mut exact, &features, &SingleThreadPool::default()) {
            Ok(_) if exact.out.len() == exact.len => Ok(exact.out),
            Ok(_) => Err(JpegError::Data("shorter than declared".into())),
            Err(_) if exact.no_memory => Err(JpegError::Memory),
            Err(e) if e.exit_code() == ExitCode::OutOfMemory => Err(JpegError::Memory),
            Err(e) => Err(JpegError::Data(e.message().to_owned())),
        }
    })
}
