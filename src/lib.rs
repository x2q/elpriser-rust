//! Client for the [elpriser.org](https://elpriser.org) electricity price API.
//!
//! **40 European price zones**, current and next hour, today and tomorrow hour by hour,
//! cheapest hours, best window, forecast days with the settled-versus-forecast flag, price
//! levels, and the Danish tariff-aware endpoints. No API key.
//!
//! * A blocking [`Client`] with a small dependency set ([`ureq`] with rustls, `chrono`,
//!   `chrono-tz`, `serde_json`); an `AsyncClient` behind the `async` cargo feature.
//! * Timeouts, retries with `Retry-After` and backoff, a proper `User-Agent`
//!   (`elpriser-rust/<version>`), typed [`Error`]s.
//! * Times: [`chrono`] with [`chrono_tz`]. [`Slot::start`] / [`Slot::end`] are UTC instants
//!   (arithmetic across daylight saving stays correct); [`Slot::local_start`], [`Slot::hour`]
//!   and [`Slot::local_date`] are the zone's own wall clock, which is also what "today" and
//!   "tomorrow" mean. A 25-hour day has 25 slots, a 23-hour day 23.
//!
//! The Python sibling is [elpriser-python](https://github.com/x2q/elpriser-python), the
//! JavaScript one [elpriser-client](https://github.com/x2q/elpriser-client).
//!
//! # Quick start
//!
//! ```no_run
//! use elpriser::{Client, Zone};
//!
//! let client = Client::new();
//!
//! // The price of the current hour, local currency per kWh.
//! if let Some(slot) = client.now(Zone::De)? {
//!     println!("{:.4} EUR/kWh, {}", slot.price.unwrap(), slot.local_start());
//! }
//!
//! // The 4 cheapest hours in the next 24 h; `kind` is Actual or Forecast.
//! for slot in client.cheapest_hours("se3", 4)? {
//!     println!("{} {:?} {}", slot.local_start(), slot.price, slot.kind);
//! }
//!
//! // The cheapest 3 consecutive hours (EV charging, dishwasher ...).
//! if let Some(window) = client.best_window(Zone::Nl, 3)? {
//!     println!("{} avg {}", window.local_start(), window.average);
//! }
//! # Ok::<(), elpriser::Error>(())
//! ```
//!
//! Every convenience method makes one request. To ask several questions fetch the series once:
//!
//! ```no_run
//! # let client = elpriser::Client::new();
//! let s = client.series("pl")?; // one HTTP request
//! s.now();
//! s.next_hour();
//! s.today();
//! s.tomorrow();
//! s.cheapest_hours(3);
//! s.best_window(2)?;
//! s.levels(); // [(slot, PriceLevel), ...] for today
//! s.forecast_days(); // only the days that are forecasts
//! println!("{} {:?} {:?}", s.currency, s.rate, s.rate_date); // "PLN", Some(4.4185), Some("2026-11-02")
//! # Ok::<(), elpriser::Error>(())
//! ```
//!
//! # What the prices are
//!
//! | Zones | Endpoint | What you get |
//! |---|---|---|
//! | `dk1`, `dk2` | `/api/forecast` | **DKK/kWh as a customer pays it**: spot + Energinet's system and transmission tariffs + electricity duty + VAT ([`Mode::InklAlt`], the default). Other [`Mode`]s, or [`Mode::NetInklAlt`] with a GLN (adds one grid company's tariff), pick a different price. Today plus nine forecast days. |
//! | the other 38 | `/api/nordic` | **Spot price only**: no VAT, duties or grid fees, so it is not what a consumer pays. Published in EUR/MWh (GBP/MWh for `gb`) and converted to local currency per kWh with the ECB rate the API sends. |
//!
//! [`Slot::price`] is always **local currency per kWh** ([`Series::currency`]). On the zone feed
//! the numbers as published are kept alongside: [`Slot::price_mwh`] (in [`Series::unit`]),
//! [`Series::rate`], [`Series::rate_date`], [`Series::rate_source`];
//! `price == price_mwh * rate / 1000`. [`Client::zone_feed`] gives the plain spot feed for a
//! Danish zone as well.
//!
//! ## Settled, forecast, estimate
//!
//! * [`PriceKind::Actual`]: the settled day-ahead price.
//! * [`PriceKind::Forecast`]: elpriser.org's **own statistical model, not an official
//!   forecast**; it is replaced by the settled price when that is published (about 13:00 CET
//!   for the next day). Forecast hours carry `min` / `max` (P10 / P90), and [`Day::horizon`]
//!   tells how many days ahead. Per-horizon back-test numbers are in [`Series::accuracy`]
//!   ([`Series::accuracy_for`], [`Series::is_low_confidence`]). The model ranks the cheapest
//!   hours better than it hits the price level. So `tomorrow()` before the auction results are
//!   out returns forecast hours: check `slot.kind`, or use [`Query::settled_only`] with
//!   `cheapest_hours_with` / `best_window_with` to use settled prices only.
//! * [`PriceKind::Estimate`]: for `gb`, `pt` and `ie` the API may attach an `estimate` for
//!   hours of a settled day that have no price yet (GB: the hours of today that have not
//!   traded; PT/IE: the last hour, 23). It is never mixed in unless you ask
//!   ([`Query::with_estimate`]).
//! * **Great Britain is not a day-ahead price**: it is the Market Index Price (within-day), an
//!   hour is priced only once it has traded, and there is no tomorrow. [`Series::now`] is
//!   therefore `None` for `gb` unless you ask for the estimate; [`Series::latest_known`]
//!   returns the newest priced hour.
//!
//! # Questions with options
//!
//! The plain methods ask about "now". Every one has a `_with` twin taking a [`Query`]: a fixed
//! moment (`at`), a day or a window, settled prices only, estimates, and the Danish mode:
//!
//! ```
//! use chrono::NaiveDate;
//! use elpriser::{Query, Series, Zone};
//! # let payload = elpriser::doc_feed();
//! # let series = Series::from_zone_feed_json(Zone::De, &payload)?;
//!
//! let day = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
//! let five = series.cheapest_hours_with(5, &Query::new().day(day).settled_only())?;
//! assert_eq!(five.len(), 5);
//! let run = series.best_window_with(3, &Query::new().day(day))?.expect("a 3-hour run");
//! assert_eq!(run.hours(), 3);
//! # Ok::<(), elpriser::Error>(())
//! ```
//!
//! Hours without a price never count; fewer than `n` hours come back if fewer are priced.
//!
//! # Price levels
//!
//! [`PriceLevel`] (`VeryCheap` ... `VeryExpensive`) ranks an hour among the priced hours of
//! its local day in quintiles, the same scale and algorithm as the `level` field of
//! `/api/shelly/tariff`. Use [`Series::levels`], [`Series::level`] or [`price_levels`].
//! [`Client::shelly_tariff`] returns the API's own levels.
//!
//! # Denmark-only endpoints
//!
//! | npm `elpriser-client` | this crate |
//! |---|---|
//! | `now()` | [`Client::dk_now_with`] (or [`Client::now`]`("dk1")` for the [`Slot`]) |
//! | `prices()` | [`Client::dk_prices_with`] |
//! | `schedule()` | [`Client::dk_schedule_with`] |
//! | `forecast()` | [`Client::forecast`]`("dk1")` / [`Series::forecast_days`] |
//! | `shellyTariff()` | [`Client::shelly_tariff`] |
//! | `supplierLookup()` | [`Client::supplier_lookup`] |
//!
//! [`Mode`] and [`Strategy`] list the values the API accepts. Invalid combinations (a `net_`
//! mode without a GLN, a `mode` on a non-Danish zone, hours out of range) return
//! [`Error::InvalidArgument`] before any request is made.
//!
//! # Errors, timeouts, retries
//!
//! ```
//! use std::time::Duration;
//! use elpriser::Client;
//!
//! let client = Client::builder()
//!     .timeout(Duration::from_secs(10))
//!     .retries(2)
//!     .backoff(Duration::from_millis(500))
//!     .user_agent("my-app/1.0")
//!     .base_url("https://elpriser.org")
//!     .build()?;
//! # Ok::<(), elpriser::Error>(())
//! ```
//!
//! Timeouts, connection errors, 429 and 5xx are retried `retries` times (the server's
//! `Retry-After` is honoured, capped at 30 s; otherwise `backoff * 2^attempt`). Other 4xx are
//! not retried. Everything returned is an [`Error`]:
//!
//! | [`Error`] variant | When |
//! |---|---|
//! | `UnknownZone` | the zone is not one of the 40 |
//! | `InvalidArgument` | a parameter was rejected before any request |
//! | `BadRequest` / `NoData` | HTTP 400 / 404 (`message` carries the API's own text) |
//! | `RateLimited` / `Server` | HTTP 429 / 5xx after the retries (`retry_after`) |
//! | `Http` | any other HTTP error status |
//! | `Timeout`, `Connection` | no HTTP answer |
//! | `Parse` | a 200 that is not the shape this version understands |
//!
//! # Data and attribution
//!
//! Prices come from elpriser.org, which aggregates: Energi Data Service (Energinet) for
//! Denmark; Bundesnetzagentur | SMARD.de via Energy-Charts.info (CC BY 4.0) for DE-LU, FR, PL,
//! IT-North, AT, BE, CH, CZ, HU and SI; the ENTSO-E Transparency Platform for ES, PT, SK, GR,
//! RO, BG, HR, RS, EE, LV, LT, ME, MK, AL, XK and the Nordic/NL zones; SEMOpx for Ireland; and
//! Elexon BMRS for Great Britain (*Contains BMRS data (c) Elexon Limited copyright and
//! database right*). Sub-hourly prices are averaged to hourly means by elpriser.org. Keep the
//! attribution if you republish the data. See <https://elpriser.org/api>. The service is free;
//! please keep request volume reasonable (responses are edge-cached for minutes, so polling
//! more often than every few minutes gains nothing).
//!
//! # Features
//!
//! * `async`: adds `AsyncClient` (reqwest with rustls, tokio for the retry sleeps).
//!
//! Minimum supported Rust version: 1.85.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

#[cfg(feature = "async")]
mod async_client;
mod client;
mod core;
mod error;
mod models;
pub mod parse;
mod transport;
mod zones;

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub use async_client::{AsyncClient, AsyncClientBuilder};
pub use client::{Client, ClientBuilder, IntoArea};
pub use error::{Error, HttpError, Result};
pub use models::{
    price_levels, Charges, Day, DayStats, DkNow, DkPrices, DkQuery, DkSchedule, Estimate,
    HourPrice, Mode, PriceKind, PriceLevel, Query, ScheduleSlot, Series, SeriesSource, Slot,
    Strategy, Supplier, TariffDays, TariffPrice, Window,
};
pub use transport::{Response, Transport, TransportError, UreqTransport};
pub use zones::{IntoZone, Zone, ZoneGroup, ZONES};

/// The version of this crate, also in the `User-Agent` (`elpriser-rust/<version>`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A small `/api/nordic` answer for the doc-tests: 10 and 11 June 2026, hourly EUR/MWh.
#[doc(hidden)]
pub fn doc_feed() -> serde_json::Value {
    use serde_json::json;
    let eur = [
        30.0, 28.0, 26.0, 25.0, 25.0, 27.0, 35.0, 50.0, 60.0, 55.0, 45.0, 40.0, 38.0, 36.0, 35.0,
        37.0, 45.0, 70.0, 90.0, 80.0, 60.0, 50.0, 40.0, 32.0,
    ];
    let day = |date: &str, kind: &str, shift: f64| {
        let prices: Vec<_> = eur
            .iter()
            .enumerate()
            .map(|(hour, p)| json!({ "hour": hour, "eur_mwh": p + shift }))
            .collect();
        json!({ "date": date, "type": kind, "prices": prices })
    };
    json!({
        "zone": "de",
        "unit": "EUR/MWh",
        "zoneInfo": { "currency": "EUR", "rate": 1.0, "rateDate": "2026-06-09", "rateSource": "ECB" },
        "days": [day("2026-06-10", "actual", 0.0), day("2026-06-11", "forecast", 5.0)],
    })
}
