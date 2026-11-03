//! Shared plumbing of the blocking and the async client: settings, request planning,
//! validation, HTTP status -> error mapping and the retry policy. No I/O happens here.

use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, HttpError, Result};
use crate::models::{DkQuery, Mode, Query, Series};
use crate::parse;
use crate::transport::Response;
use crate::zones::Zone;

pub(crate) const DEFAULT_BASE_URL: &str = "https://elpriser.org";
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_RETRIES: u32 = 2;
pub(crate) const DEFAULT_BACKOFF: Duration = Duration::from_millis(500);
pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
pub(crate) const MAX_BODY_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const RETRY_STATUSES: [u16; 5] = [429, 500, 502, 503, 504];

pub(crate) fn default_user_agent() -> String {
    format!("elpriser-rust/{}", env!("CARGO_PKG_VERSION"))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidArgument(message.into())
}

/// The options both clients share, validated.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub base_url: String,
    pub timeout: Duration,
    pub retries: u32,
    pub backoff: Duration,
    pub user_agent: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            base_url: DEFAULT_BASE_URL.into(),
            timeout: DEFAULT_TIMEOUT,
            retries: DEFAULT_RETRIES,
            backoff: DEFAULT_BACKOFF,
            user_agent: default_user_agent(),
        }
    }
}

impl Settings {
    /// Check the values; `base_url` comes back without a trailing slash.
    pub fn validated(mut self) -> Result<Settings> {
        if self.timeout.is_zero() {
            return Err(invalid("timeout must be > 0"));
        }
        self.base_url = validate_base_url(&self.base_url)?;
        Ok(self)
    }

    pub fn url(&self, request: &Request) -> String {
        build_url(&self.base_url, &request.path, &request.params)
    }

    pub fn headers(&self) -> [(&'static str, String); 2] {
        [
            ("User-Agent", self.user_agent.clone()),
            ("Accept", "application/json".into()),
        ]
    }
}

pub(crate) fn validate_base_url(base_url: &str) -> Result<String> {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"));
    let netloc = rest.map(|r| r.split(['/', '?', '#']).next().unwrap_or(""));
    match netloc {
        Some(n) if !n.is_empty() => Ok(base_url.trim_end_matches('/').to_string()),
        _ => Err(invalid(format!(
            "base_url must be an http(s) URL, got {base_url:?}"
        ))),
    }
}

/// A GET request: path plus query parameters, in order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Request {
    pub path: String,
    pub params: Vec<(String, String)>,
}

impl Request {
    fn new(path: &str) -> Request {
        Request {
            path: path.into(),
            params: Vec::new(),
        }
    }

    fn param(mut self, key: &str, value: impl Into<String>) -> Request {
        self.params.push((key.into(), value.into()));
        self
    }

    fn param_opt(self, key: &str, value: Option<String>) -> Request {
        match value {
            Some(v) => self.param(key, v),
            None => self,
        }
    }
}

/// `application/x-www-form-urlencoded` the way Python's `urlencode` does it.
fn encode_component(text: &str, out: &mut String) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

pub(crate) fn build_url(base_url: &str, path: &str, params: &[(String, String)]) -> String {
    let mut url = format!("{base_url}{path}");
    for (i, (key, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        encode_component(key, &mut url);
        url.push('=');
        encode_component(value, &mut url);
    }
    url
}

// -- validation ---------------------------------------------------------------------------

/// Validate a Danish price mode (default `inkl_alt`) and its GLN requirement.
fn to_mode(mode: Option<Mode>, gln: Option<&str>) -> Result<Mode> {
    let chosen = mode.unwrap_or(Mode::InklAlt);
    if chosen.needs_gln() && gln.is_none_or(str::is_empty) {
        return Err(invalid(format!(
            "mode {:?} needs gln - the grid tariff depends on the network company",
            chosen.as_str()
        )));
    }
    Ok(chosen)
}

fn gln_of(gln: &Option<String>) -> Option<&str> {
    gln.as_deref().filter(|g| !g.is_empty())
}

pub(crate) fn to_area(area: &str) -> Result<String> {
    let text = area.trim().to_ascii_uppercase();
    if text == "DK1" || text == "DK2" {
        Ok(text)
    } else {
        Err(invalid(format!("area must be DK1 or DK2, got {area:?}")))
    }
}

fn bounded(name: &str, value: Option<u32>, lo: u32, hi: u32) -> Result<Option<String>> {
    match value {
        None => Ok(None),
        Some(v) if (lo..=hi).contains(&v) => Ok(Some(v.to_string())),
        Some(v) => Err(invalid(format!(
            "{name} must be an integer {lo}..{hi}, got {v}"
        ))),
    }
}

/// Refuse a field the endpoint does not take.
fn reject(endpoint: &str, present: &[(&str, bool)]) -> Result<()> {
    for (name, set) in present {
        if *set {
            return Err(invalid(format!("{endpoint} does not take `{name}`")));
        }
    }
    Ok(())
}

// -- planning -----------------------------------------------------------------------------

/// What to call to get a zone's [`Series`] and how to read the answer.
#[derive(Clone, Debug)]
pub(crate) struct SeriesPlan {
    pub request: Request,
    zone: Zone,
    dk: Option<(Mode, Option<String>)>,
}

impl SeriesPlan {
    pub fn parse(&self, payload: &Value) -> Result<Series> {
        match &self.dk {
            Some((mode, gln)) => {
                parse::parse_dk_forecast(payload, self.zone, mode.as_str(), gln.as_deref())
            }
            None => parse::parse_zone_feed(payload, self.zone),
        }
    }
}

/// Danish zones default to `/api/forecast` (DKK/kWh incl. tariffs, `mode`), every other zone -
/// and any zone with `feed` set - to `/api/nordic` (EUR/MWh + ECB rate).
pub(crate) fn plan_series(zone: Zone, q: &Query, feed: bool) -> Result<SeriesPlan> {
    if let (Some(area), false) = (zone.area(), feed) {
        let gln = gln_of(&q.gln);
        let chosen = to_mode(q.mode, gln)?;
        let request = Request::new("/api/forecast")
            .param("area", area)
            .param("mode", chosen.as_str())
            .param_opt("gln", gln.map(str::to_string));
        return Ok(SeriesPlan {
            request,
            zone,
            dk: Some((chosen, gln.map(str::to_string))),
        });
    }
    if q.mode.is_some() || gln_of(&q.gln).is_some() {
        return Err(invalid(format!(
            "mode and gln only apply to the Danish tariff-aware feed (dk1/dk2, not zone_feed()); {} has plain spot prices",
            zone.key()
        )));
    }
    Ok(SeriesPlan {
        request: Request::new("/api/nordic").param("zone", zone.key()),
        zone,
        dk: None,
    })
}

fn schedule_params(request: Request, gln: Option<&str>, q: &DkQuery) -> Result<Request> {
    let mut request = request.param_opt("gln", gln.map(str::to_string));
    if let Some(strategy) = q.strategy {
        request = request.param("strategy", strategy.as_str());
    }
    for (name, value, lo, hi) in [
        ("hours", q.hours, 1, 23),
        ("pct", q.pct, 1, 100),
        ("max_off", q.max_off, 1, 12),
    ] {
        request = request.param_opt(name, bounded(name, value, lo, hi)?);
    }
    Ok(request)
}

fn dk_base(path: &str, area: &str, q: &DkQuery) -> Result<(Request, Option<String>)> {
    let area = to_area(area)?;
    let gln = gln_of(&q.gln).map(str::to_string);
    let mode = to_mode(q.mode, gln.as_deref())?;
    Ok((
        Request::new(path)
            .param("area", area)
            .param("mode", mode.as_str()),
        gln,
    ))
}

pub(crate) fn plan_dk_now(area: &str, q: &DkQuery) -> Result<Request> {
    reject(
        "dk_now",
        &[("date", q.date.is_some()), ("charges", q.charges.is_some())],
    )?;
    let (request, gln) = dk_base("/api/now", area, q)?;
    schedule_params(request, gln.as_deref(), q)
}

pub(crate) fn plan_dk_schedule(area: &str, q: &DkQuery) -> Result<Request> {
    reject("dk_schedule", &[("charges", q.charges.is_some())])?;
    let (request, gln) = dk_base("/api/schedule", area, q)?;
    let request = schedule_params(request, gln.as_deref(), q)?;
    Ok(request.param_opt("date", q.date.map(|d| d.format("%Y-%m-%d").to_string())))
}

pub(crate) fn plan_dk_prices(area: &str, q: &DkQuery) -> Result<Request> {
    reject(
        "dk_prices",
        &[
            ("strategy", q.strategy.is_some()),
            ("hours", q.hours.is_some()),
            ("pct", q.pct.is_some()),
            ("max_off", q.max_off.is_some()),
        ],
    )?;
    let (request, gln) = dk_base("/api/prices", area, q)?;
    Ok(request
        .param_opt("gln", gln)
        .param_opt("date", q.date.map(|d| d.format("%Y-%m-%d").to_string()))
        .param_opt("charges", q.charges.map(|c| c.as_str().to_string())))
}

pub(crate) fn plan_shelly_tariff(area: &str, q: &DkQuery) -> Result<Request> {
    reject(
        "shelly_tariff",
        &[
            ("strategy", q.strategy.is_some()),
            ("hours", q.hours.is_some()),
            ("pct", q.pct.is_some()),
            ("max_off", q.max_off.is_some()),
            ("date", q.date.is_some()),
            ("charges", q.charges.is_some()),
        ],
    )?;
    let (request, gln) = dk_base("/api/shelly/tariff", area, q)?;
    Ok(request.param_opt("gln", gln))
}

pub(crate) fn plan_supplier(lat: f64, lng: f64) -> Result<Request> {
    for (name, value, bound) in [("lat", lat, 90.0), ("lng", lng, 180.0)] {
        if !(-bound..=bound).contains(&value) {
            return Err(invalid(format!(
                "{name} must be a number within +-{bound}, got {value}"
            )));
        }
    }
    Ok(Request::new("/api/supplierlookup")
        .param("lat", format!("{lat:?}"))
        .param("lng", format!("{lng:?}")))
}

pub(crate) fn plan_raw(path: &str, params: &[(&str, &str)]) -> Request {
    Request {
        path: path.into(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

// -- responses ----------------------------------------------------------------------------

fn retry_after(value: Option<&String>) -> Option<Duration> {
    let seconds: f64 = value?.trim().parse().ok()?;
    // An HTTP-date does not parse as a number: the backoff applies instead.
    if seconds.is_nan() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds.min(1e9)))
}

fn error_text(resp: &Response) -> String {
    let body = String::from_utf8_lossy(&resp.body);
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&body) {
        if let Some(Value::String(message)) = map.get("error") {
            return message.clone();
        }
    }
    let text = body.trim();
    if text.is_empty() {
        "no message".into()
    } else {
        text.chars().take(200).collect()
    }
}

/// The error for a non-2xx response.
pub(crate) fn error_for(resp: &Response, url: &str) -> Error {
    let http = HttpError {
        status: resp.status,
        message: error_text(resp),
        url: url.into(),
        retry_after: retry_after(resp.headers.get("retry-after")),
    };
    match resp.status {
        400 => Error::BadRequest(http),
        404 => Error::NoData(http),
        429 => Error::RateLimited(http),
        500.. => Error::Server(http),
        _ => Error::Http(http),
    }
}

pub(crate) fn decode_json(resp: &Response, url: &str) -> Result<Value> {
    if !(200..300).contains(&resp.status) {
        return Err(error_for(resp, url));
    }
    serde_json::from_slice(&resp.body)
        .map_err(|_| Error::Parse(format!("{url} did not return JSON")))
}

pub(crate) fn is_retryable(status: u16) -> bool {
    RETRY_STATUSES.contains(&status)
}

/// How long to wait before retry number `attempt` (0-based): the server's `Retry-After` if it
/// sent one, else exponential backoff; capped.
pub(crate) fn retry_delay(
    attempt: u32,
    backoff: Duration,
    retry_after: Option<Duration>,
) -> Duration {
    match retry_after {
        Some(wait) => wait.min(MAX_RETRY_DELAY),
        None => {
            let seconds = backoff.as_secs_f64() * 2f64.powi(attempt.min(30) as i32);
            Duration::from_secs_f64(seconds.min(MAX_RETRY_DELAY.as_secs_f64()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn url_encoding_matches_python_urlencode() {
        let url = build_url(
            "https://elpriser.org",
            "/api/x",
            &[
                ("a".into(), "b c".into()),
                ("gln".into(), "57/90&=".into()),
                ("u".into(), "æ~._-".into()),
            ],
        );
        assert_eq!(
            url,
            "https://elpriser.org/api/x?a=b+c&gln=57%2F90%26%3D&u=%C3%A6~._-"
        );
        assert_eq!(build_url("http://h", "/p", &[]), "http://h/p");
    }

    #[test]
    fn base_url_validation() {
        assert_eq!(
            validate_base_url("http://localhost:8788/").unwrap(),
            "http://localhost:8788"
        );
        for bad in [
            "ftp://x",
            "file:///etc",
            "elpriser.org",
            "http://",
            "https:///x",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn retry_delay_prefers_retry_after_and_caps() {
        let b = Duration::from_millis(500);
        assert_eq!(retry_delay(0, b, None), Duration::from_millis(500));
        assert_eq!(retry_delay(1, b, None), Duration::from_secs(1));
        assert_eq!(retry_delay(2, b, None), Duration::from_secs(2));
        assert_eq!(retry_delay(40, b, None), MAX_RETRY_DELAY);
        assert_eq!(
            retry_delay(0, b, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            retry_delay(0, b, Some(Duration::from_secs(999))),
            MAX_RETRY_DELAY
        );
        assert_eq!(retry_delay(5, Duration::ZERO, None), Duration::ZERO);
    }

    #[test]
    fn retry_after_parsing() {
        let h = |v: &str| Some(v.to_string());
        assert_eq!(retry_after(h("7").as_ref()), Some(Duration::from_secs(7)));
        assert_eq!(
            retry_after(h("1.5").as_ref()),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(retry_after(h("-1").as_ref()), None);
        assert_eq!(retry_after(h("nan").as_ref()), None);
        assert_eq!(
            retry_after(h("Wed, 21 Oct 2026 07:28:00 GMT").as_ref()),
            None
        );
        assert!(retry_after(h("inf").as_ref()).is_some());
        assert_eq!(retry_after(None), None);
    }

    #[test]
    fn status_mapping() {
        let e = |s| error_for(&response(s, &[], r#"{"error":"nope"}"#), "u");
        assert!(matches!(e(400), Error::BadRequest(_)));
        assert!(matches!(e(404), Error::NoData(_)));
        assert!(matches!(e(429), Error::RateLimited(_)));
        assert!(matches!(e(503), Error::Server(_)));
        assert!(matches!(e(403), Error::Http(_)));
        assert!(matches!(e(418), Error::Http(_)));
        assert_eq!(e(400).http().unwrap().message, "nope");
        assert_eq!(e(404).status(), Some(404));
    }

    #[test]
    fn error_text_falls_back_to_the_body() {
        let r = response(403, &[], "<html>Cloudflare says no</html>");
        assert_eq!(error_text(&r), "<html>Cloudflare says no</html>");
        assert_eq!(error_text(&response(500, &[], "")), "no message");
        let long = "x".repeat(500);
        assert_eq!(error_text(&response(500, &[], &long)).len(), 200);
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let r = response(200, &[], "<html>captive portal</html>");
        assert!(
            matches!(decode_json(&r, "u"), Err(Error::Parse(m)) if m.contains("did not return JSON"))
        );
    }

    #[test]
    fn plans() {
        let q = Query::new();
        let p = plan_series(Zone::Se3, &q, false).unwrap();
        assert_eq!(p.request, plan_raw("/api/nordic", &[("zone", "se3")]));
        let p = plan_series(Zone::Dk1, &q, false).unwrap();
        assert_eq!(
            p.request,
            plan_raw("/api/forecast", &[("area", "DK1"), ("mode", "inkl_alt")])
        );
        let p = plan_series(Zone::Dk1, &q, true).unwrap();
        assert_eq!(p.request.path, "/api/nordic");
        assert!(plan_series(Zone::De, &Query::new().mode(Mode::SpotEx), false).is_err());
        assert!(plan_series(Zone::Dk1, &Query::new().mode(Mode::NetInklAlt), false).is_err());
        assert!(plan_series(
            Zone::Dk1,
            &Query::new().mode(Mode::NetInklAlt).gln(""),
            false
        )
        .is_err());
    }
}
