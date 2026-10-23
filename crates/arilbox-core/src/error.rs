//! Error type for `arilbox-core`.

/// Errors produced by `arilbox-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An identifier failed validation. The rejected value is deliberately not included, so
    /// hostile input never ends up in logs or API messages verbatim.
    #[error("invalid {kind}: {reason}")]
    InvalidName {
        /// What was being validated, e.g. `container name`.
        kind: &'static str,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// The configuration file could not be read, parsed or is inconsistent.
    #[error("config error: {0}")]
    Config(String),

    /// An I/O error while reading host information.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Result alias for `arilbox-core`.
pub type Result<T, E = Error> = std::result::Result<T, E>;
