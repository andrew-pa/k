//! Kernel-wide error type.

use alloc::{boxed::Box, string::String};
use kapi::ErrorCode;
use snafu::Snafu;

use crate::{fs::FsError, memory::MemoryError, registry::RegistryError, storage::StorageError};

/// Errors that can happen in the kernel.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    /// Error occurred in memory subsystem.
    Memory {
        reason: String,
        #[snafu(source(from(MemoryError, Box::new)))]
        source: Box<MemoryError>,
    },
    /// Error occurred in registry subsystem.
    Registry {
        reason: String,
        #[snafu(source(from(RegistryError, Box::new)))]
        source: Box<RegistryError>,
    },
    /// Error occurred in storage driver.
    Storage {
        reason: String,
        #[snafu(source(from(StorageError, Box::new)))]
        source: Box<StorageError>,
    },
    /// Error occurred in file system driver.
    FileSystem {
        reason: String,
        #[snafu(source(from(FsError, Box::new)))]
        source: Box<FsError>,
    },

    /// A value was expected when none was found.
    ExpectedValue { reason: String },

    /// Miscellaneous error occurred.
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
}
