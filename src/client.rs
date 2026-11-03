//! The blocking client.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::core::{self, Request, Settings};
use crate::error::{Error, Result};
use crate::models::{
    Day, DkNow, DkPrices, DkQuery, DkSchedule, PriceLevel, Query, Series, Slot, Supplier,
    TariffDays, Window,
};
use crate::parse;
use crate::transport::{Transport, TransportError, UreqTransport};
use crate::zones::{IntoZone, Zone};

type Sleeper = Arc<dyn Fn(Duration) + Send + Sync>;

/// Builder for a [`Client`]; see [`Client::builder`].
///
/// ```
/// use std::time::Duration;
/// use elpriser::Client;
///
/// let client = Client::builder()
///     .timeout(Duration::from_secs(5))
///     .retries(1)
///     .user_agent("my-app/1.0")
///     .build()?;
/// assert_eq!(client.user_agent(), "my-app/1.0");
/// # Ok::<(), elpriser::Error>(())
/// ```
pub struct ClientBuilder {
    settings: Settings,
    transport: Option<Arc<dyn Transport>>,
}

impl ClientBuilder {
    /// API root, default `https://elpriser.org`.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.settings.base_url = base_url.into();
        self
    }

    /// Time allowed per attempt (default 10 s, must be above zero).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.settings.timeout = timeout;
        self
    }

    /// Extra attempts after a timeout, a connection error, 429 or 5xx (default 2).
    pub fn retries(mut self, retries: u32) -> Self {
        self.settings.retries = retries;
        self
    }

    /// Base delay between retries (default 0.5 s): `backoff * 2^attempt`, unless the server
    /// sent `Retry-After`.
    pub fn backoff(mut self, backoff: Duration) -> Self {
        self.settings.backoff = backoff;
        self
    }

    /// Replace `elpriser-rust/<version>`. The API's CDN rejects some default agents, so keep
    /// a custom one (`"my-app/1.0"` is fine).
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.settings.user_agent = user_agent.into();
        self
    }

    /// Replace the HTTP layer, see [`Transport`].
    pub fn transport(mut self, transport: impl Transport) -> Self {
        self.transport = Some(Arc::new(transport));
        self
    }

    /// Check the settings and build the client.
    pub fn build(self) -> Result<Client> {
        Ok(Client {
            settings: self.settings.validated()?,
            transport: self
                .transport
                .unwrap_or_else(|| Arc::new(UreqTransport::new())),
            sleeper: Arc::new(std::thread::sleep),
        })
    }
}

/// Blocking client for the elpriser.org API.
///
/// Cheap to clone (the connection pool and settings are shared). Every convenience method makes
/// one request; to ask several questions fetch the [`Series`] once with
/// [`series`](Client::series).
///
/// ```no_run
/// use elpriser::{Client, Zone};
///
/// let client = Client::new();
///
/// if let Some(slot) = client.now(Zone::De)? {
///     println!("{} EUR/kWh at {}", slot.price.unwrap(), slot.local_start());
/// }
/// for slot in client.cheapest_hours("se3", 4)? {
///     println!("{} {:?} {}", slot.local_start(), slot.price, slot.kind);
/// }
/// # Ok::<(), elpriser::Error>(())
/// ```
#[derive(Clone)]
pub struct Client {
    settings: Settings,
    transport: Arc<dyn Transport>,
    sleeper: Sleeper,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.settings.base_url)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Client::new()
    }
}

impl Client {
    /// A client with the defaults: `https://elpriser.org`, 10 s timeout, 2 retries.
    pub fn new() -> Client {
        Client::builder()
            .build()
            .expect("the default settings are valid")
    }

    /// Start configuring a client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder {
            settings: Settings::default(),
            transport: None,
        }
    }

    /// The API root.
    pub fn base_url(&self) -> &str {
        &self.settings.base_url
    }

    /// The `User-Agent` this client sends.
    pub fn user_agent(&self) -> &str {
        &self.settings.user_agent
    }

    #[cfg(test)]
    pub(crate) fn with_sleeper(
        mut self,
        sleeper: impl Fn(Duration) + Send + Sync + 'static,
    ) -> Client {
        self.sleeper = Arc::new(sleeper);
        self
    }

    // -- HTTP ---------------------------------------------------------------------------

    fn fetch(&self, request: &Request) -> Result<Value> {
        let s = &self.settings;
        let url = s.url(request);
        let owned = s.headers();
        let headers: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut attempt = 0;
        loop {
            let mut retry_after = None;
            let failure = match self.transport.get(&url, &headers, s.timeout) {
                Err(TransportError::Timeout(detail)) => Error::Timeout {
                    url: url.clone(),
                    timeout: s.timeout,
                    detail,
                },
                Err(TransportError::Connection(detail)) => Error::Connection {
                    url: url.clone(),
                    detail,
                },
                Ok(resp) if core::is_retryable(resp.status) && attempt < s.retries => {
                    let failure = core::error_for(&resp, &url);
                    retry_after = failure.retry_after();
                    failure
                }
                Ok(resp) => return core::decode_json(&resp, &url),
            };
            if attempt >= s.retries {
                return Err(failure);
            }
            (self.sleeper)(core::retry_delay(attempt, s.backoff, retry_after));
            attempt += 1;
        }
    }

    /// Escape hatch: `GET` any API path and return the decoded JSON (no parsing).
    ///
    /// ```no_run
    /// # let client = elpriser::Client::new();
    /// let spec = client.get_json("/api/openapi.json", &[])?;
    /// # Ok::<(), elpriser::Error>(())
    /// ```
    pub fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.fetch(&core::plan_raw(path, params))
    }

    // -- prices (any of the 40 zones) ---------------------------------------------------

    /// Fetch a zone's whole price series in one request.
    ///
    /// Everything else ([`now`](Client::now), [`cheapest_hours`](Client::cheapest_hours) ...)
    /// is this plus a method on the result: call `series()` once and ask it several questions.
    ///
    /// * Danish zones (`dk1`/`dk2`): `/api/forecast`, DKK/kWh incl. Energinet's tariffs,
    ///   electricity duty and VAT (mode `inkl_alt` unless [`Query::mode`] says otherwise;
    ///   `net_` modes need a GLN), today plus nine forecast days.
    /// * Every other zone: `/api/nordic`, the spot price (no VAT, duties or grid fees) in
    ///   EUR/MWh converted to the local currency with the ECB rate; settled days plus
    ///   forecast days (where the zone has a forecast); `estimate` for `gb`/`pt`/`ie`.
    pub fn series(&self, zone: impl IntoZone) -> Result<Series> {
        self.series_with(zone, &Query::default())
    }

    /// Like [`series`](Client::series); `q.mode` and `q.gln` pick the Danish price.
    pub fn series_with(&self, zone: impl IntoZone, q: &Query) -> Result<Series> {
        let plan = core::plan_series(zone.into_zone()?, q, false)?;
        plan.parse(&self.fetch(&plan.request)?)
    }

    /// Like [`series`](Client::series), but always the plain spot-price feed `/api/nordic` -
    /// also for `dk1`/`dk2` (EUR/MWh, no Danish tariffs or taxes).
    pub fn zone_feed(&self, zone: impl IntoZone) -> Result<Series> {
        let plan = core::plan_series(zone.into_zone()?, &Query::default(), true)?;
        plan.parse(&self.fetch(&plan.request)?)
    }

    /// The price of the current hour, or `None` when this hour has no price (yet).
    pub fn now(&self, zone: impl IntoZone) -> Result<Option<Slot>> {
        self.now_with(zone, &Query::default())
    }

    /// [`now`](Client::now) with options: `at`, `with_estimate()` (fills `gb`/`pt`/`ie`
    /// gaps), `mode` / `gln`.
    pub fn now_with(&self, zone: impl IntoZone, q: &Query) -> Result<Option<Slot>> {
        Ok(self.series_with(zone, q)?.now_with(q))
    }

    /// The price of the next hour (a forecast hour if tomorrow is not published yet).
    pub fn next_hour(&self, zone: impl IntoZone) -> Result<Option<Slot>> {
        self.next_hour_with(zone, &Query::default())
    }

    /// [`next_hour`](Client::next_hour) with options.
    pub fn next_hour_with(&self, zone: impl IntoZone, q: &Query) -> Result<Option<Slot>> {
        Ok(self.series_with(zone, q)?.next_hour_with(q))
    }

    /// Today's hourly prices in the zone's local day.
    pub fn today(&self, zone: impl IntoZone) -> Result<Vec<Slot>> {
        self.today_with(zone, &Query::default())
    }

    /// [`today`](Client::today) with options.
    pub fn today_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Slot>> {
        Ok(self.series_with(zone, q)?.today_with(q))
    }

    /// Tomorrow's hourly prices; `Forecast` hours until the day-ahead prices are published.
    pub fn tomorrow(&self, zone: impl IntoZone) -> Result<Vec<Slot>> {
        self.tomorrow_with(zone, &Query::default())
    }

    /// [`tomorrow`](Client::tomorrow) with options.
    pub fn tomorrow_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Slot>> {
        Ok(self.series_with(zone, q)?.tomorrow_with(q))
    }

    /// The `n` cheapest hours within the next 24 h, in time order.
    /// See [`Series::cheapest_hours`].
    pub fn cheapest_hours(&self, zone: impl IntoZone, n: usize) -> Result<Vec<Slot>> {
        self.cheapest_hours_with(zone, n, &Query::default())
    }

    /// [`cheapest_hours`](Client::cheapest_hours) with a day or window, `settled_only()` and so
    /// on. See [`Series::cheapest_hours_with`].
    pub fn cheapest_hours_with(
        &self,
        zone: impl IntoZone,
        n: usize,
        q: &Query,
    ) -> Result<Vec<Slot>> {
        self.series_with(zone, q)?.cheapest_hours_with(n, q)
    }

    /// The cheapest run of `hours` consecutive hours within the next 24 h.
    /// See [`Series::best_window`].
    pub fn best_window(&self, zone: impl IntoZone, hours: usize) -> Result<Option<Window>> {
        self.best_window_with(zone, hours, &Query::default())
    }

    /// [`best_window`](Client::best_window) with options. See [`Series::best_window_with`].
    pub fn best_window_with(
        &self,
        zone: impl IntoZone,
        hours: usize,
        q: &Query,
    ) -> Result<Option<Window>> {
        self.series_with(zone, q)?.best_window_with(hours, q)
    }

    /// Only the forecast days (`day.kind == PriceKind::Forecast`; `day.horizon` where given).
    ///
    /// The zone feed's forecast is elpriser.org's own statistical model - not an official
    /// forecast - and is replaced by the settled price when that is published.
    pub fn forecast(&self, zone: impl IntoZone) -> Result<Vec<Day>> {
        self.forecast_with(zone, &Query::default())
    }

    /// [`forecast`](Client::forecast) with options (`mode` / `gln` for Danish zones).
    pub fn forecast_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Day>> {
        Ok(self
            .series_with(zone, q)?
            .forecast_days()
            .into_iter()
            .cloned()
            .collect())
    }

    /// Today's hours with their quintile [`PriceLevel`].
    pub fn levels(&self, zone: impl IntoZone) -> Result<Vec<(Slot, PriceLevel)>> {
        self.levels_with(zone, &Query::default())
    }

    /// [`levels`](Client::levels) for `q.day` (default: the day of `q.at`).
    pub fn levels_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<(Slot, PriceLevel)>> {
        Ok(self.series_with(zone, q)?.levels_with(q))
    }

    // -- Denmark-only endpoints (npm client parity) -----------------------------------

    /// `/api/now`: this hour's DKK/kWh and whether the schedule strategy says ON now.
    /// `area` is `"DK1"` or `"DK2"` (or [`Zone::Dk1`] / [`Zone::Dk2`]).
    pub fn dk_now(&self, area: impl IntoArea) -> Result<DkNow> {
        self.dk_now_with(area, &DkQuery::default())
    }

    /// [`dk_now`](Client::dk_now) with a mode, GLN and strategy (see [`DkQuery`]).
    pub fn dk_now_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkNow> {
        let request = core::plan_dk_now(&area.into_area(), q)?;
        parse::parse_dk_now(&self.fetch(&request)?)
    }

    /// `/api/prices`: the 24 hourly prices of today (Danish time).
    pub fn dk_prices(&self, area: impl IntoArea) -> Result<DkPrices> {
        self.dk_prices_with(area, &DkQuery::default())
    }

    /// [`dk_prices`](Client::dk_prices) for a date, mode, GLN, `charges` (see [`DkQuery`]).
    pub fn dk_prices_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkPrices> {
        let request = core::plan_dk_prices(&area.into_area(), q)?;
        parse::parse_dk_prices(&self.fetch(&request)?)
    }

    /// `/api/schedule`: the 24 h ON/OFF plan of the server's default strategy.
    pub fn dk_schedule(&self, area: impl IntoArea) -> Result<DkSchedule> {
        self.dk_schedule_with(area, &DkQuery::default())
    }

    /// [`dk_schedule`](Client::dk_schedule) with a strategy (e.g. `cheapest_n` with
    /// `hours(5)`), mode, GLN and date (see [`DkQuery`]).
    pub fn dk_schedule_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkSchedule> {
        let request = core::plan_dk_schedule(&area.into_area(), q)?;
        parse::parse_dk_schedule(&self.fetch(&request)?)
    }

    /// `/api/shelly/tariff`: today + tomorrow in Tibber's shape, with the API's own quintile
    /// `level`.
    pub fn shelly_tariff(&self, area: impl IntoArea) -> Result<TariffDays> {
        self.shelly_tariff_with(area, &DkQuery::default())
    }

    /// [`shelly_tariff`](Client::shelly_tariff) with a mode and GLN (see [`DkQuery`]).
    pub fn shelly_tariff_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<TariffDays> {
        let request = core::plan_shelly_tariff(&area.into_area(), q)?;
        parse::parse_shelly_tariff(&self.fetch(&request)?)
    }

    /// `/api/supplierlookup`: the Danish grid company (netselskab) at a coordinate.
    pub fn supplier_lookup(&self, lat: f64, lng: f64) -> Result<Supplier> {
        let request = core::plan_supplier(lat, lng)?;
        parse::parse_supplier(&self.fetch(&request)?)
    }
}

/// A Danish area: `"DK1"`, `"dk2"`, [`Zone::Dk1`] ... Anything else is an
/// [`Error::InvalidArgument`] when the request is planned.
pub trait IntoArea {
    /// The text of the area.
    fn into_area(self) -> String;
}

impl IntoArea for Zone {
    fn into_area(self) -> String {
        self.key().to_string()
    }
}

impl IntoArea for &Zone {
    fn into_area(self) -> String {
        self.key().to_string()
    }
}

impl IntoArea for &str {
    fn into_area(self) -> String {
        self.to_string()
    }
}

impl IntoArea for String {
    fn into_area(self) -> String {
        self
    }
}

impl IntoArea for &String {
    fn into_area(self) -> String {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::transport::Response;

    fn feed() -> Response {
        Response::new(200, crate::doc_feed().to_string())
    }

    fn failing(status: u16, retry_after: Option<&str>) -> Response {
        let r = Response::new(status, r#"{"error":"boom"}"#);
        match retry_after {
            Some(v) => r.with_header("Retry-After", v),
            None => r,
        }
    }

    type Item = Result<Response, TransportError>;

    /// A client answering from a queue (the last item repeats) that records its sleeps
    /// instead of sleeping.
    fn client(
        items: Vec<Item>,
        retries: u32,
        backoff: Duration,
    ) -> (Client, Arc<Mutex<Vec<Duration>>>) {
        let queue = Mutex::new(VecDeque::from(items));
        let transport = move |_: &str, _: &[(&str, &str)], _: Duration| -> Item {
            let mut q = queue.lock().unwrap();
            if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q[0].clone()
            }
        };
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let recorded = sleeps.clone();
        let client = Client::builder()
            .transport(transport)
            .retries(retries)
            .backoff(backoff)
            .build()
            .unwrap()
            .with_sleeper(move |d| recorded.lock().unwrap().push(d));
        (client, sleeps)
    }

    #[test]
    fn backoff_doubles_between_retries() {
        let items = vec![Ok(failing(503, None)), Ok(failing(502, None)), Ok(feed())];
        let (client, sleeps) = client(items, 2, Duration::from_millis(500));
        assert_eq!(client.series("de").unwrap().zone, Zone::De);
        assert_eq!(
            *sleeps.lock().unwrap(),
            [Duration::from_millis(500), Duration::from_secs(1)]
        );
    }

    #[test]
    fn retry_after_is_honoured_and_capped() {
        let items = vec![
            Ok(failing(429, Some("3"))),
            Ok(failing(429, Some("999"))),
            Ok(feed()),
        ];
        let (client, sleeps) = client(items, 2, Duration::from_millis(100));
        client.series("de").unwrap();
        assert_eq!(
            *sleeps.lock().unwrap(),
            [Duration::from_secs(3), core::MAX_RETRY_DELAY]
        );
    }

    #[test]
    fn gives_up_with_the_last_failure_and_its_retry_after() {
        let (client, sleeps) = client(vec![Ok(failing(503, Some("7")))], 2, Duration::ZERO);
        let err = client.series("de").unwrap_err();
        assert!(matches!(err, Error::Server(_)));
        assert_eq!(err.retry_after(), Some(Duration::from_secs(7)));
        assert_eq!(sleeps.lock().unwrap().len(), 2);
    }

    #[test]
    fn timeouts_and_connection_errors_back_off_too() {
        let items = vec![
            Err(TransportError::Timeout("t".into())),
            Err(TransportError::Connection("c".into())),
            Ok(feed()),
        ];
        let (client, sleeps) = client(items, 2, Duration::from_millis(250));
        client.series("de").unwrap();
        assert_eq!(
            *sleeps.lock().unwrap(),
            [Duration::from_millis(250), Duration::from_millis(500)]
        );
    }

    #[test]
    fn client_errors_are_never_slept_on() {
        let (client, sleeps) = client(vec![Ok(failing(404, None))], 5, Duration::from_secs(1));
        assert!(matches!(client.series("de").unwrap_err(), Error::NoData(_)));
        assert!(sleeps.lock().unwrap().is_empty());
    }
}
