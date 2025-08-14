use std::error::Error;
use std::fmt;
use std::io;

/// 自定义错误类型
#[derive(Debug)]
pub enum NeoError {
    Io(io::Error),
    SessionClosed,
    Base64Decode(base64::DecodeError),
    Other(String),
}

impl fmt::Display for NeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NeoError::Io(e) => write!(f, "IO error: {}", e),
            NeoError::SessionClosed => write!(f, "Session is closed"),
            NeoError::Base64Decode(e) => write!(f, "Base64 decode error: {}", e),
            NeoError::Other(s) => write!(f, "Error: {}", s),
        }
    }
}

impl Error for NeoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NeoError::Io(e) => Some(e),
            NeoError::Base64Decode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for NeoError {
    fn from(e: io::Error) -> Self {
        NeoError::Io(e)
    }
}

impl From<base64::DecodeError> for NeoError {
    fn from(e: base64::DecodeError) -> Self {
        NeoError::Base64Decode(e)
    }
}
