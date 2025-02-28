use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{Error as ioError, ErrorKind};

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct RettyErrorKind {
    pub kind: ErrorKind,
    pub message: String,
}

impl Display for RettyErrorKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} , {}", self.kind, self.message)
    }
}

impl Error for RettyErrorKind {}

impl From<ioError> for RettyErrorKind {
    fn from(e: ioError) -> Self {
        RettyErrorKind {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}

impl RettyErrorKind {
    pub fn new(kind: ErrorKind, message: String) -> Self {
        RettyErrorKind { kind, message }
    }
}
