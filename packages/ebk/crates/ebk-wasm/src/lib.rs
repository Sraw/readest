//! The EBK reader for `wasm32-unknown-unknown`, with a plain C interface: no generated bindings.
//!
//! One instance of the module reads one file. The file stays with the host, which hands over the
//! bytes the reader asks for (`host_read`), so a book is never in memory as a whole. Every call
//! returns a status; what it produced - a member's bytes, the member list, or the text of an
//! error - is then at `ebk_result_ptr()`, `ebk_result_len()` until the next call.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use ebk::{Error, Reader, Source};

#[link(wasm_import_module = "env")]
extern "C" {
    /// Copies `len` bytes of the file, starting at `offset`, to `ptr`. Returns 0 when it did.
    fn host_read(offset: f64, ptr: *mut u8, len: usize) -> i32;
}

struct Host {
    len: u64,
}

impl Source for Host {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        // offsets are below 2^53: the host measured the file with a JavaScript number
        match unsafe { host_read(offset as f64, buf.as_mut_ptr(), buf.len()) } {
            0 => Ok(()),
            _ => Err(std::io::Error::other("the host could not read the file")),
        }
    }
}

#[derive(Default)]
struct State {
    reader: Option<Reader<Host>>,
    result: Vec<u8>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

const OK: i32 = 0;
const INVALID: i32 = 1;
const CORRUPT: i32 = 2;
const TOO_LARGE: i32 = 3;
const UNSUPPORTED: i32 = 4;
const NO_SUCH_MEMBER: i32 = 5;
const IO: i32 = 6;
const NOT_OPEN: i32 = 7;

fn finish(state: &mut State, outcome: Result<Vec<u8>, Error>) -> i32 {
    let (status, result) = match outcome {
        Ok(data) => (OK, data),
        Err(e) => {
            let status = match e {
                Error::Invalid(_) => INVALID,
                Error::Corrupt(_) => CORRUPT,
                Error::TooLarge(_) => TOO_LARGE,
                Error::Unsupported(_) => UNSUPPORTED,
                Error::NoSuchMember(_) => NO_SUCH_MEMBER,
                Error::Io(_) => IO,
            };
            (status, e.to_string().into_bytes())
        }
    };
    state.result = result;
    status
}

/// Opens the file of `file_len` bytes that `host_read` reads. `member_limit` is the longest member
/// that will be read, in bytes; `pixel_limit` the largest recompressed JPEG, in pixels.
#[no_mangle]
pub extern "C" fn ebk_open(file_len: f64, member_limit: f64, pixel_limit: f64) -> i32 {
    STATE.with_borrow_mut(|state| {
        state.reader = None;
        let opened = Reader::open(Host { len: file_len as u64 }).map(|mut reader| {
            reader.set_member_limit(member_limit as u64);
            reader.set_pixel_limit(pixel_limit as u64);
            state.reader = Some(reader);
            Vec::new()
        });
        finish(state, opened)
    })
}

/// The member table as text, one member per line: storage mode, length, stored length, whether this
/// reader can read it (1 or 0), and the path, separated by tabs. A path holds no tab or line break.
#[no_mangle]
pub extern "C" fn ebk_members() -> i32 {
    STATE.with_borrow_mut(|state| {
        let Some(reader) = &state.reader else { return NOT_OPEN };
        state.result = Vec::new();
        let mut text = String::new();
        for (i, m) in reader.members().enumerate() {
            use std::fmt::Write;
            // room for the numbers and the path, asked for in a way that can fail: the table can be tens of megabytes
            if text.try_reserve(m.path.len() + 64).is_err() {
                state.result = b"not enough memory for the member table".to_vec();
                return TOO_LARGE;
            }
            let _ = writeln!(text, "{}\t{}\t{}\t{}\t{}", m.mode.to_u8(), m.raw_len, m.stored_len, u8::from(reader.readable(i)), m.path);
        }
        state.result = text.into_bytes();
        OK
    })
}

/// Reads member number `index`, checked against its length and CRC-32.
#[no_mangle]
pub extern "C" fn ebk_read(index: u32) -> i32 {
    STATE.with_borrow_mut(|state| {
        // the result of the call before is given up first: it can be as large as a member
        state.result = Vec::new();
        let Some(reader) = &mut state.reader else { return NOT_OPEN };
        let outcome = reader.read(index as usize);
        finish(state, outcome)
    })
}

/// After a member could not be read for lack of memory, lets the text blocks concerned be tried again.
#[no_mangle]
pub extern "C" fn ebk_retry() {
    STATE.with_borrow_mut(|state| {
        if let Some(reader) = &mut state.reader {
            reader.retry_starved();
        }
    })
}

#[no_mangle]
pub extern "C" fn ebk_result_ptr() -> *const u8 {
    STATE.with_borrow(|state| state.result.as_ptr())
}

#[no_mangle]
pub extern "C" fn ebk_result_len() -> usize {
    STATE.with_borrow(|state| state.result.len())
}
