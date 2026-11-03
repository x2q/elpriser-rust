//! The async client (feature `async`): the same methods as [`Client`](crate::Client), built on
//! reqwest (rustls) and tokio.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::client::IntoArea;
use crate::core::{self, Request, Settings};
use crate::error::{Error, Result};
use crate::models::{
    Day, DkNow, DkPrices, DkQuery, DkSchedule, PriceLevel, Query, Series, Slot, Supplier,
    TariffDays, Window,
};
use crate::parse;
use crate::transport::{Response, TransportError};
use crate::zones::IntoZone;

/// Builder for an [`AsyncClient`]; see [`AsyncClient::builder`].
pub struct AsyncClientBuilder {
    settings: Settings,
    http: Option<reqwest::Client>,
}

impl AsyncClientBuilder {
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

    /// Replace `elpriser-rust/<version>`.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.settings.user_agent = user_agent.into();
        self
    }

    /// Bring your own [`reqwest::Client`] (proxy, TLS roots ...).
    pub fn http_client(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    /// Check the settings and build the client.
    pub fn build(self) -> Result<AsyncClient> {
        let http = match self.http {
            Some(http) => http,
            None => reqwest::Client::builder().build().map_err(|e| {
                Error::InvalidArgument(format!("cannot build the HTTP client: {e}"))
            })?,
        };
        Ok(AsyncClient {
            settings: self.settings.validated()?,
            http,
        })
    }
}

/// Async client for the elpriser.org API (feature `async`).
///
/// Same methods as [`Client`](crate::Client), all `async`. Needs a tokio runtime.
///
/// ```no_run
/// # async fn run() -> Result<(), elpriser::Error> {
/// use elpriser::AsyncClient;
///
/// let client = AsyncClient::new();
/// println!("{:?}", client.now("fr").await?);
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct AsyncClient {
    settings: Settings,
    http: reqwest::Client,
}

impl fmt::Debug for AsyncClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncClient")
            .field("base_url", &self.settings.base_url)
            .finish_non_exhaustive()
    }
}

impl Default for AsyncClient {
    fn default() -> Self {
        AsyncClient::new()
    }
}

impl AsyncClient {
    /// A client with the defaults: `https://elpriser.org`, 10 s timeout, 2 retries.
    pub fn new() -> AsyncClient {
        AsyncClient::builder()
            .build()
            .expect("the default settings are valid")
    }

    /// Start configuring a client.
    pub fn builder() -> AsyncClientBuilder {
        AsyncClientBuilder {
            settings: Settings::default(),
            http: None,
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

    async fn attempt(&self, url: &str) -> std::result::Result<Response, TransportError> {
        let s = &self.settings;
        let mut request = self.http.get(url).timeout(s.timeout);
        for (name, value) in s.headers() {
            request = request.header(name, value);
        }
        let map = |e: reqwest::Error| {
            if e.is_timeout() {
                TransportError::Timeout(e.to_string())
            } else {
                TransportError::Connection(e.to_string())
            }
        };
        let mut response = request.send().await.map_err(map)?;
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
        while let Some(chunk) = response.chunk().await.map_err(map)? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > core::MAX_BODY_BYTES {
                return Err(TransportError::Connection("response too large".into()));
            }
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    async fn fetch(&self, request: &Request) -> Result<Value> {
        let s = &self.settings;
        let url = s.url(request);
        let mut attempt = 0;
        loop {
            let mut retry_after = None;
            let failure = match self.attempt(&url).await {
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
            tokio::time::sleep(core::retry_delay(attempt, s.backoff, retry_after)).await;
            attempt += 1;
        }
    }

    /// Escape hatch: `GET` any API path and return the decoded JSON (no parsing).
    pub async fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.fetch(&core::plan_raw(path, params)).await
    }

    /// Fetch a zone's whole price series in one request. See
    /// [`Client::series`](crate::Client::series).
    pub async fn series(&self, zone: impl IntoZone) -> Result<Series> {
        self.series_with(zone, &Query::default()).await
    }

    /// Like [`series`](AsyncClient::series); `q.mode` and `q.gln` pick the Danish price.
    pub async fn series_with(&self, zone: impl IntoZone, q: &Query) -> Result<Series> {
        let plan = core::plan_series(zone.into_zone()?, q, false)?;
        plan.parse(&self.fetch(&plan.request).await?)
    }

    /// Always the plain spot-price feed `/api/nordic`, also for `dk1`/`dk2`.
    pub async fn zone_feed(&self, zone: impl IntoZone) -> Result<Series> {
        let plan = core::plan_series(zone.into_zone()?, &Query::default(), true)?;
        plan.parse(&self.fetch(&plan.request).await?)
    }

    /// The price of the current hour, or `None` when this hour has no price (yet).
    pub async fn now(&self, zone: impl IntoZone) -> Result<Option<Slot>> {
        self.now_with(zone, &Query::default()).await
    }

    /// [`now`](AsyncClient::now) with options.
    pub async fn now_with(&self, zone: impl IntoZone, q: &Query) -> Result<Option<Slot>> {
        Ok(self.series_with(zone, q).await?.now_with(q))
    }

    /// The price of the next hour.
    pub async fn next_hour(&self, zone: impl IntoZone) -> Result<Option<Slot>> {
        self.next_hour_with(zone, &Query::default()).await
    }

    /// [`next_hour`](AsyncClient::next_hour) with options.
    pub async fn next_hour_with(&self, zone: impl IntoZone, q: &Query) -> Result<Option<Slot>> {
        Ok(self.series_with(zone, q).await?.next_hour_with(q))
    }

    /// Today's hourly prices in the zone's local day.
    pub async fn today(&self, zone: impl IntoZone) -> Result<Vec<Slot>> {
        self.today_with(zone, &Query::default()).await
    }

    /// [`today`](AsyncClient::today) with options.
    pub async fn today_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Slot>> {
        Ok(self.series_with(zone, q).await?.today_with(q))
    }

    /// Tomorrow's hourly prices; `Forecast` hours until the day-ahead prices are published.
    pub async fn tomorrow(&self, zone: impl IntoZone) -> Result<Vec<Slot>> {
        self.tomorrow_with(zone, &Query::default()).await
    }

    /// [`tomorrow`](AsyncClient::tomorrow) with options.
    pub async fn tomorrow_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Slot>> {
        Ok(self.series_with(zone, q).await?.tomorrow_with(q))
    }

    /// The `n` cheapest hours within the next 24 h, in time order.
    pub async fn cheapest_hours(&self, zone: impl IntoZone, n: usize) -> Result<Vec<Slot>> {
        self.cheapest_hours_with(zone, n, &Query::default()).await
    }

    /// [`cheapest_hours`](AsyncClient::cheapest_hours) with a day or window and so on.
    pub async fn cheapest_hours_with(
        &self,
        zone: impl IntoZone,
        n: usize,
        q: &Query,
    ) -> Result<Vec<Slot>> {
        self.series_with(zone, q).await?.cheapest_hours_with(n, q)
    }

    /// The cheapest run of `hours` consecutive hours within the next 24 h.
    pub async fn best_window(&self, zone: impl IntoZone, hours: usize) -> Result<Option<Window>> {
        self.best_window_with(zone, hours, &Query::default()).await
    }

    /// [`best_window`](AsyncClient::best_window) with options.
    pub async fn best_window_with(
        &self,
        zone: impl IntoZone,
        hours: usize,
        q: &Query,
    ) -> Result<Option<Window>> {
        self.series_with(zone, q).await?.best_window_with(hours, q)
    }

    /// Only the forecast days.
    pub async fn forecast(&self, zone: impl IntoZone) -> Result<Vec<Day>> {
        self.forecast_with(zone, &Query::default()).await
    }

    /// [`forecast`](AsyncClient::forecast) with options.
    pub async fn forecast_with(&self, zone: impl IntoZone, q: &Query) -> Result<Vec<Day>> {
        Ok(self
            .series_with(zone, q)
            .await?
            .forecast_days()
            .into_iter()
            .cloned()
            .collect())
    }

    /// Today's hours with their quintile [`PriceLevel`].
    pub async fn levels(&self, zone: impl IntoZone) -> Result<Vec<(Slot, PriceLevel)>> {
        self.levels_with(zone, &Query::default()).await
    }

    /// [`levels`](AsyncClient::levels) for `q.day` (default: the day of `q.at`).
    pub async fn levels_with(
        &self,
        zone: impl IntoZone,
        q: &Query,
    ) -> Result<Vec<(Slot, PriceLevel)>> {
        Ok(self.series_with(zone, q).await?.levels_with(q))
    }

    /// `/api/now`. See [`Client::dk_now`](crate::Client::dk_now).
    pub async fn dk_now(&self, area: impl IntoArea) -> Result<DkNow> {
        self.dk_now_with(area, &DkQuery::default()).await
    }

    /// [`dk_now`](AsyncClient::dk_now) with a mode, GLN and strategy.
    pub async fn dk_now_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkNow> {
        let request = core::plan_dk_now(&area.into_area(), q)?;
        parse::parse_dk_now(&self.fetch(&request).await?)
    }

    /// `/api/prices`.
    pub async fn dk_prices(&self, area: impl IntoArea) -> Result<DkPrices> {
        self.dk_prices_with(area, &DkQuery::default()).await
    }

    /// [`dk_prices`](AsyncClient::dk_prices) for a date, mode, GLN, `charges`.
    pub async fn dk_prices_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkPrices> {
        let request = core::plan_dk_prices(&area.into_area(), q)?;
        parse::parse_dk_prices(&self.fetch(&request).await?)
    }

    /// `/api/schedule`.
    pub async fn dk_schedule(&self, area: impl IntoArea) -> Result<DkSchedule> {
        self.dk_schedule_with(area, &DkQuery::default()).await
    }

    /// [`dk_schedule`](AsyncClient::dk_schedule) with a strategy, mode, GLN and date.
    pub async fn dk_schedule_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<DkSchedule> {
        let request = core::plan_dk_schedule(&area.into_area(), q)?;
        parse::parse_dk_schedule(&self.fetch(&request).await?)
    }

    /// `/api/shelly/tariff`.
    pub async fn shelly_tariff(&self, area: impl IntoArea) -> Result<TariffDays> {
        self.shelly_tariff_with(area, &DkQuery::default()).await
    }

    /// [`shelly_tariff`](AsyncClient::shelly_tariff) with a mode and GLN.
    pub async fn shelly_tariff_with(&self, area: impl IntoArea, q: &DkQuery) -> Result<TariffDays> {
        let request = core::plan_shelly_tariff(&area.into_area(), q)?;
        parse::parse_shelly_tariff(&self.fetch(&request).await?)
    }

    /// `/api/supplierlookup`.
    pub async fn supplier_lookup(&self, lat: f64, lng: f64) -> Result<Supplier> {
        let request = core::plan_supplier(lat, lng)?;
        parse::parse_supplier(&self.fetch(&request).await?)
    }
}
