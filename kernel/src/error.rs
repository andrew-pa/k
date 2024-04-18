//! Kernel-wide error type.

use core::str::Utf8Error;

use alloc::{boxed::Box, string::String};
use kapi::completions::ErrorCode;
use snafu::Snafu;

use crate::{
    fs::FsError,
    memory::{self, MemoryError},
    registry::RegistryError,
    storage::StorageError,
};

/// Errors that can happen in the kernel.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    /// Error occurred in memory subsystem.
    #[snafu(display("Memory error: {reason}"))]
    Memory {
        reason: String,
        #[snafu(source(from(MemoryError, Box::new)))]
        source: Box<MemoryError>,
    },
    /// Error occurred in registry subsystem.
    #[snafu(display("Registry error: {reason}"))]
    Registry {
        reason: String,
        #[snafu(source(from(RegistryError, Box::new)))]
        source: Box<RegistryError>,
    },
    /// Error occurred in storage driver.
    #[snafu(display("Storage error: {reason}"))]
    Storage {
        reason: String,
        #[snafu(source(from(StorageError, Box::new)))]
        source: Box<StorageError>,
    },
    /// Error occurred in file system driver.
    #[snafu(display("Filesystem error: {reason}"))]
    FileSystem {
        reason: String,
        #[snafu(source(from(FsError, Box::new)))]
        source: Box<FsError>,
    },

    /// A string passed to the kernel was not valid UTF-8.
    #[snafu(display("UTF-8 error: {reason}"))]
    Utf8 {
        reason: String,
        #[snafu(source(from(Utf8Error, Box::new)))]
        source: Box<Utf8Error>,
    },

    /// An error occurred with an additional reason from the caller.
    #[snafu(display("{reason}"))]
    Inner {
        reason: String,
        #[snafu(source(from(Error, Box::new)))]
        source: Box<Error>,
    },

    /// Miscellaneous error occurred with no underlying source error.
    #[snafu(display("{reason} (code = {code:?})"))]
    Misc {
        reason: String,
        code: Option<ErrorCode>,
    },

    /// Miscellaneous error occurred due to some underlying error.
    #[snafu(display("{reason} (code = {code:?})"))]
    Other {
        reason: String,
        /// Specific error code to return to user space, if possible.
        code: Option<ErrorCode>,
        source: Box<dyn snafu::Error + Send + Sync + 'static>,
    },
}

impl Error {
    pub fn other<S, E>(reason: S, code: Option<ErrorCode>, src: E) -> Self
    where
        String: From<S>,
        E: snafu::Error + Send + Sync + 'static,
    {
        Error::Other {
            reason: reason.into(),
            code,
            source: Box::new(src),
        }
    }

    pub fn as_code(&self) -> ErrorCode {
        match self {
            Error::Memory { source, .. } => match **source {
                MemoryError::OutOfMemory | MemoryError::InsufficentForAllocation { .. } => {
                    ErrorCode::OutOfMemory
                }
                MemoryError::Map {
                    source: memory::paging::MapError::InvalidTag,
                }
                | MemoryError::Map {
                    source: memory::paging::MapError::CopyFromUnmapped { .. },
                } => ErrorCode::InvalidPointer,
                _ => ErrorCode::Internal,
            },
            Error::Registry { source, .. } => match **source {
                RegistryError::NotFound { .. } => ErrorCode::NotFound,
                RegistryError::Unsupported => ErrorCode::UnsupportedOperation,
                RegistryError::InvalidPath => ErrorCode::BadFormat,
                RegistryError::HandlerAlreadyRegistered { .. } => ErrorCode::Internal,
            },
            Error::Storage { source, .. } => match **source {
                StorageError::BadVector { .. } => ErrorCode::BadFormat,
                StorageError::DeviceError { .. } => ErrorCode::Device,
                StorageError::MemoryError { .. } => todo!(),
            },
            Error::FileSystem { source, .. } => match **source {
                FsError::OutOfBounds { .. } => ErrorCode::OutOfBounds,
                _ => ErrorCode::Internal,
            },
            Error::Utf8 { .. } => ErrorCode::InvalidUtf8,
            Error::Misc {
                code: Some(code), ..
            } => *code,
            Error::Other {
                code: Some(code), ..
            } => *code,
            Error::Inner { source, .. } => source.as_code(),
            _ => ErrorCode::Internal,
        }
    }
}
