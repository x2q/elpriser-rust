//! The HTTP layer, replaceable for tests or proxies.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use crate::core::MAX_BODY_BYTES;

/// What a [`Transport`] hands back: status, lower-cased headers, raw body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// The response headers, names lower-cased.
    pub headers: HashMap<String, String>,
    /// The body.
    pub body: Vec<u8>,
}

impl Response {
    /// A response with a status and a body and no headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            headers: HashMap::new(),
            body: body.into(),
        }
    }

    /// Add a header (the name is lower-cased).
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.insert(name.to_ascii_lowercase(), value.into());
        self
    }
}

/// The request never produced an HTTP answer. The client retries these.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The attempt timed out.
    #[error("{0}")]
    Timeout(String),
    /// DNS, TLS, refused, reset, body too large ...
    #[error("{0}")]
    Connection(String),
}

/// One `GET`: `get(url, headers, timeout) -> Response`.
///
/// An HTTP error status is *not* an error here: return it as a [`Response`] and the client maps
/// it (and decides about retrying). Return [`TransportError`] only when there was no HTTP
/// answer. Closures with the matching signature are transports too, which makes a fake easy:
///
/// ```
/// use elpriser::{Client, Response};
///
/// let client = Client::builder()
///     .transport(|_url: &str, _headers: &[(&str, &str)], _timeout: std::time::Duration| {
///         Ok(Response::new(404, r#"{"error":"no prices yet"}"#))
///     })
///     .build()?;
/// let err = client.now("de").unwrap_err();
/// assert_eq!(err.status(), Some(404));
/// # Ok::<(), elpriser::Error>(())
/// ```
pub trait Transport: Send + Sync + 'static {
    /// Perform the request.
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Response, TransportError>;
}

impl<F> Transport for F
where
    F: Fn(&str, &[(&str, &str)], Duration) -> Result<Response, TransportError>
        + Send
        + Sync
        + 'static,
{
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Response, TransportError> {
        self(url, headers, timeout)
    }
}

/// The default transport: one `GET` through [`ureq`] (rustls).
#[derive(Clone, Debug)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// A transport with its own connection pool.
    pub fn new() -> UreqTransport {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        UreqTransport { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        UreqTransport::new()
    }
}

impl Transport for UreqTransport {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Response, TransportError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .map_err(map_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().ok()?.to_string(),
                ))
            })
            .collect();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_BODY_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::TimedOut => TransportError::Timeout(e.to_string()),
                _ => TransportError::Connection(e.to_string()),
            })?;
        if body.len() as u64 > MAX_BODY_BYTES {
            return Err(TransportError::Connection("response too large".into()));
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

fn map_error(err: ureq::Error) -> TransportError {
    match err {
        ureq::Error::Timeout(_) => TransportError::Timeout(err.to_string()),
        ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::TimedOut => {
            TransportError::Timeout(err.to_string())
        }
        _ => TransportError::Connection(err.to_string()),
    }
}
