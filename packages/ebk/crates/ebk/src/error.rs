use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The file as a whole is not a valid EBK file (header, footer or index).
    Invalid(String),
    /// One member cannot be read; other members may still be readable.
    Corrupt(String),
    /// The member uses a storage mode this reader does not implement.
    Unsupported(String),
    /// Something is larger than this reader's limit, or than the memory available; the file may still be valid.
    TooLarge(String),
    /// There is no member with this number.
    NoSuchMember(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Invalid(m) => write!(f, "not a valid EBK file: {m}"),
            Error::Corrupt(m) => write!(f, "corrupt member: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::TooLarge(m) => write!(f, "too large: {m}"),
            Error::NoSuchMember(i) => write!(f, "there is no member number {i}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}
