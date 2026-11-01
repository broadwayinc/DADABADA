use std::fmt;

/// An error returned to a client: HTTP status, a stable code, and a plain message.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into() }
    }
    pub fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(400, code, message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "INVALID_REQUEST", message)
    }
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(403, "NO_ACCESS", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "NOT_FOUND", message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(401, "UNAUTHORIZED", message)
    }
    pub fn exists(message: impl Into<String>) -> Self {
        Self::new(409, "EXISTS", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "INTERNAL", message)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<rocksdb::Error> for ApiError {
    fn from(e: rocksdb::Error) -> Self {
        ApiError::internal(format!("storage error: {e}"))
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::internal(format!("file error: {e}"))
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
