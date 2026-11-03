//! The error type. Everything this crate returns on purpose is an [`Error`].

use std::time::Duration;

/// `Result` with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What the API said when it answered with an HTTP error status.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpError {
    /// The HTTP status code.
    pub status: u16,
    /// The API's own `error` text when it sent one, otherwise the start of the body.
    pub message: String,
    /// The URL that was requested.
    pub url: String,
    /// The server's `Retry-After` (in seconds) on a 429 or 5xx, if it sent one.
    pub retry_after: Option<Duration>,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HTTP {} from {}: {}",
            self.status, self.url, self.message
        )
    }
}

/// Every error this crate returns on purpose.
///
/// Network and HTTP failures are returned only after the configured retries are used up.
///
/// ```
/// use elpriser::{Error, Zone};
///
/// let err = "narnia".parse::<Zone>().unwrap_err();
/// assert!(matches!(err, Error::UnknownZone(_)));
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The zone is not one of the zones elpriser.org serves (see [`Zone`](crate::Zone)).
    #[error("{0}")]
    UnknownZone(String),

    /// A parameter was rejected before any request was made (a `net_` mode without a GLN,
    /// hours out of range, a bad base URL ...).
    #[error("{0}")]
    InvalidArgument(String),

    /// HTTP 400: the API rejected a parameter (unknown zone, mode, bad date ...).
    #[error("{0}")]
    BadRequest(HttpError),

    /// HTTP 404: the API has no prices for this request (yet).
    #[error("{0}")]
    NoData(HttpError),

    /// HTTP 429, after the retries. [`HttpError::retry_after`] has the server's hint.
    #[error("{0}")]
    RateLimited(HttpError),

    /// HTTP 5xx, after the retries. A 503 may carry [`HttpError::retry_after`].
    #[error("{0}")]
    Server(HttpError),

    /// Any other HTTP error status (403 from a CDN, for example).
    #[error("{0}")]
    Http(HttpError),

    /// The request timed out (after the configured retries).
    #[error("timed out after {timeout:?}: {url} ({detail})")]
    Timeout {
        /// The URL that was requested.
        url: String,
        /// The per-attempt timeout that was configured.
        timeout: Duration,
        /// What the transport said.
        detail: String,
    },

    /// The request never produced an HTTP answer: DNS, TLS, refused, reset ... (after the
    /// configured retries).
    #[error("could not reach {url}: {detail}")]
    Connection {
        /// The URL that was requested.
        url: String,
        /// What the transport said.
        detail: String,
    },

    /// The API answered 200 but the body is not the shape this version understands.
    #[error("{0}")]
    Parse(String),
}

impl Error {
    /// The HTTP details, for the variants that come from an HTTP error status.
    pub fn http(&self) -> Option<&HttpError> {
        match self {
            Error::BadRequest(e)
            | Error::NoData(e)
            | Error::RateLimited(e)
            | Error::Server(e)
            | Error::Http(e) => Some(e),
            _ => None,
        }
    }

    /// The HTTP status code, if the API answered with an error status.
    pub fn status(&self) -> Option<u16> {
        self.http().map(|e| e.status)
    }

    /// The server's `Retry-After`, if it sent one.
    pub fn retry_after(&self) -> Option<Duration> {
        self.http().and_then(|e| e.retry_after)
    }

    /// True for the network-level failures (timeout or no HTTP answer at all).
    pub fn is_network(&self) -> bool {
        matches!(self, Error::Timeout { .. } | Error::Connection { .. })
    }
}
