//! Typed result objects and the price arithmetic that works on them.
//!
//! Nothing here does I/O. A [`Series`] is one parsed API answer; every question you can ask
//! about prices (current hour, cheapest hours, best window, levels ...) is a method on it, so
//! one HTTP request can answer many questions.
//!
//! Money: [`Slot::price`] is always **local currency per kWh** ([`Series::currency`]). For the
//! zone feed (`/api/nordic`) the API publishes EUR/MWh (GBP/MWh for `gb`) plus an ECB rate;
//! [`Slot::price_mwh`] is the number as published and `price = price_mwh * rate / 1000`. For the
//! Danish tariff-aware feed (`/api/forecast`) the API publishes DKK/kWh directly.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::zones::Zone;

/// Decimals the client rounds computed prices to (like the Python SDK).
pub(crate) const DECIMALS: usize = 5;

/// Round half-to-even on the exact decimal value, the way Python's `round(x, 5)` does.
pub(crate) fn round5(value: f64) -> f64 {
    format!("{value:.prec$}", prec = DECIMALS)
        .parse()
        .unwrap_or(value)
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident, $what:literal, $plural:literal {
        $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every value, in the API's order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The text the API uses.
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<$name> {
                $name::ALL.iter().copied().find(|v| v.as_str() == s).ok_or_else(|| {
                    let valid: Vec<&str> = $name::ALL.iter().map(|v| v.as_str()).collect();
                    Error::InvalidArgument(format!(
                        "unknown {} {:?}; valid {}: {}", $what, s, $plural, valid.join(", ")
                    ))
                })
            }
        }
    };
}

string_enum! {
    /// What a price is.
    PriceKind, "price kind", "price kinds" {
        /// Settled day-ahead price (for `gb`: the within-day market index).
        Actual => "actual",
        /// elpriser.org's own statistical forecast. NOT an official forecast; it is replaced by
        /// the settled price when that is published.
        Forecast => "forecast",
        /// elpriser.org's estimate of an hour of a settled day that has no price yet (`gb`,
        /// `pt`, `ie`). Only appears when you ask for it.
        Estimate => "estimate",
    }
}

string_enum! {
    /// Quintile rank of an hour among the priced hours of its local day.
    ///
    /// Same scale and same method as the Tibber-compatible `level` field of
    /// `/api/shelly/tariff`: the cheapest fifth of the day is `VeryCheap` and so on.
    PriceLevel, "price level", "price levels" {
        /// The cheapest fifth of the day.
        VeryCheap => "VERY_CHEAP",
        /// The second fifth.
        Cheap => "CHEAP",
        /// The middle fifth (also what an unpriced hour gets).
        Normal => "NORMAL",
        /// The fourth fifth.
        Expensive => "EXPENSIVE",
        /// The dearest fifth of the day.
        VeryExpensive => "VERY_EXPENSIVE",
    }
}

string_enum! {
    /// Which Danish price `/api/forecast` and friends return (DK1/DK2 only).
    Mode, "mode", "modes" {
        /// Raw spot price, excluding VAT.
        SpotEx => "spot_ex",
        /// Spot incl. 25 % VAT.
        SpotInkl => "spot_inkl",
        /// Spot + Energinet system and transmission tariffs + electricity duty, incl. VAT.
        /// Not the local grid tariff (use [`Mode::NetInklAlt`] + a GLN). The default.
        InklAlt => "inkl_alt",
        /// `InklAlt` with the reduced duty for electric heating.
        InklAltElvarme => "inkl_alt_elvarme",
        /// `InklAlt` without the electricity duty.
        InklAltMinus => "inkl_alt_minus",
        /// `InklAlt` plus a specific grid company's tariff (needs a GLN).
        NetInklAlt => "net_inkl_alt",
        /// Like `NetInklAlt` with the reduced duty for electric heating (needs a GLN).
        NetInklAltElvarme => "net_inkl_alt_elvarme",
        /// Grid tariff included but without the electricity duty (needs a GLN).
        NetInklTarif => "net_inkl_tarif",
    }
}

impl Mode {
    /// The `net_` modes include one grid company's tariff and so need a GLN.
    pub const fn needs_gln(self) -> bool {
        matches!(
            self,
            Mode::NetInklAlt | Mode::NetInklAltElvarme | Mode::NetInklTarif
        )
    }
}

string_enum! {
    /// Server-side schedule strategies of `/api/schedule` and `/api/now`.
    Strategy, "strategy", "strategies" {
        /// The `hours` cheapest hours are ON.
        CheapestN => "cheapest_n",
        /// The cheapest `pct` percent of the hours are ON.
        CheapestPct => "cheapest_pct",
        /// Everything but the `hours` dearest hours is ON.
        AvoidExpensiveN => "avoid_expensive_n",
        /// Everything but the dearest `pct` percent is ON.
        AvoidExpensivePct => "avoid_expensive_pct",
        /// OFF during the daily peak.
        AvoidPeak => "avoid_peak",
        /// ON at night.
        NightCheap => "night_cheap",
        /// The server's combined strategy.
        Smart => "smart",
    }
}

string_enum! {
    /// Which tax and tariff rates a past date is priced with on `/api/prices`.
    Charges, "charges value", "values" {
        /// The rates that applied on that date.
        Historical => "historical",
        /// Today's rates.
        Current => "current",
    }
}

/// Where a [`Series`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesSource {
    /// `/api/nordic`: the spot price in EUR/MWh (GBP/MWh for `gb`) plus an ECB rate.
    ZoneFeed,
    /// `/api/forecast`: DKK/kWh with Danish tariffs and taxes.
    DkForecast,
}

// --------------------------------------------------------------------------
// Hours
// --------------------------------------------------------------------------

/// One hour of price.
///
/// `start` and `end` are **UTC** instants, so comparing, sorting and subtracting them is always
/// right, also across a daylight-saving change where local datetimes would silently be wrong.
/// Use [`local_start`](Slot::local_start) / [`local_end`](Slot::local_end) (or
/// [`hour`](Slot::hour) / [`local_date`](Slot::local_date)) for the zone's own wall clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// Start of the hour (UTC).
    pub start: DateTime<Utc>,
    /// End of the hour (UTC).
    pub end: DateTime<Utc>,
    /// Local currency per kWh ([`Series::currency`]); `None` = no price (yet).
    pub price: Option<f64>,
    /// Whether this is a settled price, a model forecast or an estimate.
    pub kind: PriceKind,
    /// P10, local currency per kWh. Only on model forecast/estimate hours.
    pub min: Option<f64>,
    /// P90, local currency per kWh. Only on model forecast/estimate hours.
    pub max: Option<f64>,
    /// The number as the zone feed publishes it ([`Series::unit`], e.g. EUR/MWh).
    /// `None` on the Danish tariff-aware feed, which is published in DKK/kWh.
    pub price_mwh: Option<f64>,
    /// P10 as published (zone feed only).
    pub min_mwh: Option<f64>,
    /// P90 as published (zone feed only).
    pub max_mwh: Option<f64>,
    /// The zone's wall clock.
    pub tz: Tz,
}

impl Slot {
    /// `start` on the zone's wall clock. Do not subtract these across a DST change; use
    /// `start` / `end` for arithmetic.
    pub fn local_start(&self) -> DateTime<Tz> {
        self.start.with_timezone(&self.tz)
    }

    /// `end` on the zone's wall clock.
    pub fn local_end(&self) -> DateTime<Tz> {
        self.end.with_timezone(&self.tz)
    }

    /// The local wall-clock hour (0-23), as the API's `hour` field (on a 25-hour day the
    /// doubled hour 2 appears twice).
    pub fn hour(&self) -> u32 {
        self.local_start().hour()
    }

    /// The local calendar date.
    pub fn local_date(&self) -> NaiveDate {
        self.local_start().date_naive()
    }

    /// True when the hour has a price.
    pub fn is_priced(&self) -> bool {
        self.price.is_some()
    }
}

/// One local calendar day as the API delivered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// The local date.
    pub date: NaiveDate,
    /// `Actual` for a settled day, `Forecast` for a model day (`Estimate` never appears here:
    /// estimates are kept apart in [`Series::estimate`]).
    pub kind: PriceKind,
    /// The hours, in time order (23, 24 or 25 of them).
    pub slots: Vec<Slot>,
    /// Zone feed, forecast days of EU zones: days after the last settled day + 1 (2-7; `gb`
    /// starts at 1).
    pub horizon: Option<i64>,
    /// Danish feed only: `0` = Sunday .. `6` = Saturday, as the API sends it.
    pub weekday: Option<i64>,
    /// Danish feed only: `"model"` or `"heuristic"` (weekday-average fallback) on forecast days.
    pub source: Option<String>,
    /// Danish `net_` modes: `"published"` or `"last_known"`.
    pub tariff: Option<String>,
    /// Danish `net_` modes: the grid company's tariff for each local hour (as the API sends
    /// it: DKK/kWh excl. VAT; `None` where it publishes none).
    pub grid_tariff: Option<Vec<Option<f64>>>,
}

impl Day {
    /// True for a model day.
    pub fn is_forecast(&self) -> bool {
        self.kind == PriceKind::Forecast
    }

    /// True for a settled day.
    pub fn is_settled(&self) -> bool {
        self.kind == PriceKind::Actual
    }

    /// Every hour of the day has a price.
    pub fn is_complete(&self) -> bool {
        !self.slots.is_empty() && self.slots.iter().all(|s| s.price.is_some())
    }
}

/// The API's top-level `estimate` (`gb`, `pt`, `ie`).
///
/// elpriser.org's own statistical estimate for hours of a settled day that have no price yet
/// (`gb`: the hours of today that have not traded; `pt`/`ie`: the last local hour, 23).
/// [`Series::days`] is never changed by it.
#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    /// The local date the estimate is for.
    pub date: NaiveDate,
    /// The API's `scope`, e.g. `"partial-day"`.
    pub scope: String,
    /// The model's name.
    pub model: Option<String>,
    /// When the estimate was made.
    pub generated_at: Option<DateTime<Utc>>,
    /// The estimated hours (`kind` is `Estimate`).
    pub slots: Vec<Slot>,
    /// The API's description of how it was made.
    pub method: Option<String>,
    /// The API's back-test numbers, unchanged.
    pub accuracy: Option<Value>,
}

/// Lowest, highest and mean price of a local day.
#[derive(Clone, Debug, PartialEq)]
pub struct DayStats {
    /// The cheapest hour (earliest wins a tie).
    pub minimum: Slot,
    /// The dearest hour (earliest wins a tie).
    pub maximum: Slot,
    /// Mean price, local currency per kWh.
    pub average: f64,
    /// How many hours are priced.
    pub count: usize,
}

/// A run of consecutive hours (see [`Series::best_window`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// Start of the first hour (UTC).
    pub start: DateTime<Utc>,
    /// End of the last hour (UTC).
    pub end: DateTime<Utc>,
    /// The hours of the window.
    pub slots: Vec<Slot>,
    /// Mean price per kWh over the window (local currency).
    pub average: f64,
}

impl Window {
    /// Start on the zone's wall clock.
    pub fn local_start(&self) -> DateTime<Tz> {
        self.slots[0].local_start()
    }

    /// End on the zone's wall clock.
    pub fn local_end(&self) -> DateTime<Tz> {
        self.slots[self.slots.len() - 1].local_end()
    }

    /// Number of hours.
    pub fn hours(&self) -> usize {
        self.slots.len()
    }
}

// --------------------------------------------------------------------------
// Level calculation (same algorithm as /api/shelly/tariff)
// --------------------------------------------------------------------------

/// Quintile levels for one day's prices, in the same order.
///
/// Mirrors the API: cut points are the sorted values at 20/40/60/80 % (index
/// `floor(n * f)`, clamped), `p <= cut` decides. A missing price is `Normal` (as in the API);
/// a day without any price is all `Normal`.
///
/// ```
/// use elpriser::{price_levels, PriceLevel};
///
/// let prices: Vec<Option<f64>> = (1..=10).map(|p| Some(p as f64)).collect();
/// let levels = price_levels(&prices);
/// assert_eq!(levels[0], PriceLevel::VeryCheap);
/// assert_eq!(levels[4], PriceLevel::Cheap);
/// assert_eq!(levels[9], PriceLevel::VeryExpensive);
/// ```
pub fn price_levels(prices: &[Option<f64>]) -> Vec<PriceLevel> {
    let mut valid: Vec<f64> = prices.iter().flatten().copied().collect();
    if valid.is_empty() {
        return vec![PriceLevel::Normal; prices.len()];
    }
    valid.sort_by(f64::total_cmp);
    let at = |f: f64| {
        let idx = ((valid.len() as f64 * f).floor() as usize).min(valid.len() - 1);
        valid[idx]
    };
    let (p20, p40, p60, p80) = (at(0.2), at(0.4), at(0.6), at(0.8));
    prices
        .iter()
        .map(|p| match p {
            None => PriceLevel::Normal,
            Some(p) if *p <= p20 => PriceLevel::VeryCheap,
            Some(p) if *p <= p40 => PriceLevel::Cheap,
            Some(p) if *p <= p60 => PriceLevel::Normal,
            Some(p) if *p <= p80 => PriceLevel::Expensive,
            Some(_) => PriceLevel::VeryExpensive,
        })
        .collect()
}

// --------------------------------------------------------------------------
// Query: the knobs of the questions
// --------------------------------------------------------------------------

/// Options for a question about prices: which moment, which window, what to count.
///
/// Everything is optional; [`Query::new`] (also [`Default`]) asks about "now", the next 24
/// hours, settled and forecast prices, no estimates. `mode` and `gln` only matter for the
/// client methods that fetch a Danish zone (they pick the Danish price, see [`Mode`]); the
/// [`Series`] methods ignore them.
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use elpriser::Query;
///
/// let q = Query::new()
///     .at(Utc.with_ymd_and_hms(2026, 11, 3, 7, 20, 0).unwrap())
///     .settled_only();
/// assert!(!q.include_forecast);
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Query {
    /// The moment "now" means (default: the real clock).
    pub at: Option<DateTime<Utc>>,
    /// A local calendar day to look at (cheapest hours / best window / levels). Cannot be
    /// combined with `start` / `end`.
    pub day: Option<NaiveDate>,
    /// Start of an explicit window (default: the start of the current hour).
    pub start: Option<DateTime<Utc>>,
    /// End of an explicit window (default: `start` + 24 hours).
    pub end: Option<DateTime<Utc>>,
    /// Count model forecast hours (default `true`).
    pub include_forecast: bool,
    /// Let estimates fill hours that have no price yet (`gb`, `pt`, `ie`; default `false`).
    pub include_estimate: bool,
    /// Danish zones: the price mode to fetch (default [`Mode::InklAlt`]).
    pub mode: Option<Mode>,
    /// Danish zones: the grid company's GLN for the `net_` modes.
    pub gln: Option<String>,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            at: None,
            day: None,
            start: None,
            end: None,
            include_forecast: true,
            include_estimate: false,
            mode: None,
            gln: None,
        }
    }
}

impl Query {
    /// The default query.
    pub fn new() -> Query {
        Query::default()
    }

    /// Ask as of this moment instead of now.
    pub fn at<T: TimeZone>(mut self, at: DateTime<T>) -> Query {
        self.at = Some(at.with_timezone(&Utc));
        self
    }

    /// Look at one local calendar day.
    pub fn day(mut self, day: NaiveDate) -> Query {
        self.day = Some(day);
        self
    }

    /// Look at the window `start..end`.
    pub fn between<T: TimeZone, U: TimeZone>(
        mut self,
        start: DateTime<T>,
        end: DateTime<U>,
    ) -> Query {
        self.start = Some(start.with_timezone(&Utc));
        self.end = Some(end.with_timezone(&Utc));
        self
    }

    /// Start the window here (it ends 24 hours later unless [`end`](Query::end) is set).
    pub fn start<T: TimeZone>(mut self, start: DateTime<T>) -> Query {
        self.start = Some(start.with_timezone(&Utc));
        self
    }

    /// End the window here.
    pub fn end<T: TimeZone>(mut self, end: DateTime<T>) -> Query {
        self.end = Some(end.with_timezone(&Utc));
        self
    }

    /// Use settled prices only (ignore model forecast hours).
    pub fn settled_only(mut self) -> Query {
        self.include_forecast = false;
        self
    }

    /// Whether to count model forecast hours.
    pub fn include_forecast(mut self, yes: bool) -> Query {
        self.include_forecast = yes;
        self
    }

    /// Let estimates fill unpriced hours (`gb`, `pt`, `ie`).
    pub fn with_estimate(self) -> Query {
        self.include_estimate(true)
    }

    /// Whether estimates may fill unpriced hours (`gb`, `pt`, `ie`).
    pub fn include_estimate(mut self, yes: bool) -> Query {
        self.include_estimate = yes;
        self
    }

    /// Danish zones: pick the price mode.
    pub fn mode(mut self, mode: Mode) -> Query {
        self.mode = Some(mode);
        self
    }

    /// Danish zones: the grid company's GLN (needed by the `net_` modes).
    pub fn gln(mut self, gln: impl Into<String>) -> Query {
        self.gln = Some(gln.into());
        self
    }

    fn moment(&self) -> DateTime<Utc> {
        self.at.unwrap_or_else(Utc::now)
    }
}

fn floor_hour(moment: DateTime<Utc>) -> DateTime<Utc> {
    moment
        .with_minute(0)
        .and_then(|m| m.with_second(0))
        .and_then(|m| m.with_nanosecond(0))
        .unwrap_or(moment)
}

// --------------------------------------------------------------------------
// The series
// --------------------------------------------------------------------------

/// Everything one API call returned for a zone, plus the questions you ask of it.
///
/// Fetch it once with [`Client::series`](crate::Client::series) and ask it several questions:
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use elpriser::{Query, Series, Zone};
/// # let payload = elpriser::doc_feed();
///
/// // `payload` is the JSON of one `/api/nordic?zone=de` answer.
/// let series = Series::from_zone_feed_json(Zone::De, &payload)?;
/// let q = Query::new().at(Utc.with_ymd_and_hms(2026, 6, 10, 10, 20, 0).unwrap());
///
/// let now = series.now_with(&q).unwrap();
/// assert_eq!(now.hour(), 12); // 10:20 UTC is 12:20 in Berlin
/// let cheapest = series.cheapest_hours_with(3, &q)?;
/// assert_eq!(cheapest.len(), 3);
/// let window = series.best_window_with(2, &q)?.unwrap();
/// assert_eq!(window.hours(), 2);
/// # Ok::<(), elpriser::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Series {
    /// The zone.
    pub zone: Zone,
    /// ISO 4217 code the prices are in (`zoneInfo.currency`; DKK on the Danish feed).
    pub currency: String,
    /// What the API published: `"EUR/MWh"`, `"GBP/MWh"` (`gb`) or `"DKK/kWh"`.
    pub unit: String,
    /// The days the API delivered, oldest first.
    pub days: Vec<Day>,
    /// The API's estimate for hours of a settled day without a price (`gb`, `pt`, `ie`).
    pub estimate: Option<Estimate>,
    /// Which endpoint this came from.
    pub source: SeriesSource,
    /// Danish feed: the price mode that was asked for.
    pub mode: Option<String>,
    /// Danish feed: the GLN that was asked for.
    pub gln: Option<String>,
    /// ECB reference rate EUR -> `currency` (`zoneInfo.rate`; 1 for EUR zones and for `gb`,
    /// whose prices are published in GBP). `None` on the Danish feed.
    pub rate: Option<f64>,
    /// The date of the ECB rate.
    pub rate_date: Option<String>,
    /// Where the rate comes from (`"ECB"`).
    pub rate_source: Option<String>,
    /// When the API generated the answer.
    pub generated: Option<String>,
    /// How fresh the forecast model's training data is.
    pub trained_through: Option<String>,
    /// The forecast model: a name (zone feed) or the API's description (Danish feed).
    pub model: Option<Value>,
    /// When the forecast was generated.
    pub forecast_generated: Option<String>,
    /// The API's back-test statistics for the forecast days, unchanged (`byHorizon` ...).
    pub accuracy: Option<Value>,
    /// Danish feed: `{"sys", "trans", "afg", "vat"}` as the API sends them.
    pub charges: Option<Value>,
    /// The API's answer, unchanged.
    pub raw: Value,
}

impl Series {
    /// Parse a recorded `/api/nordic?zone=...` answer (see [`parse`](crate::parse)).
    pub fn from_zone_feed_json(zone: Zone, payload: &Value) -> Result<Series> {
        crate::parse::parse_zone_feed(payload, zone)
    }

    /// Parse a recorded `/api/forecast?area=...` answer for a Danish zone.
    pub fn from_dk_forecast_json(
        zone: Zone,
        payload: &Value,
        mode: Mode,
        gln: Option<&str>,
    ) -> Result<Series> {
        crate::parse::parse_dk_forecast(payload, zone, mode.as_str(), gln)
    }

    // -- basics ---------------------------------------------------------------------------

    /// The zone's wall clock.
    pub fn tz(&self) -> Tz {
        self.zone.tz()
    }

    /// Unit of [`Slot::price`], e.g. `"SEK/kWh"`.
    pub fn price_unit(&self) -> String {
        format!("{}/kWh", self.currency)
    }

    /// Every hour in time order. With `include_estimate` an estimated hour replaces the
    /// unpriced settled hour it belongs to (kind `Estimate`).
    pub fn all_slots(&self, include_estimate: bool) -> Vec<Slot> {
        let mut by_start: BTreeMap<DateTime<Utc>, Slot> = BTreeMap::new();
        for day in &self.days {
            for slot in &day.slots {
                by_start.insert(slot.start, *slot);
            }
        }
        if include_estimate {
            if let Some(est) = &self.estimate {
                for slot in &est.slots {
                    if let Some(current) = by_start.get(&slot.start) {
                        if current.price.is_none() && slot.price.is_some() {
                            by_start.insert(slot.start, *slot);
                        }
                    }
                }
            }
        }
        by_start.into_values().collect()
    }

    /// The [`Day`] for a local date, if the API delivered it.
    pub fn day(&self, when: NaiveDate) -> Option<&Day> {
        self.days.iter().find(|d| d.date == when)
    }

    /// The settled days.
    pub fn settled_days(&self) -> Vec<&Day> {
        self.days.iter().filter(|d| d.is_settled()).collect()
    }

    /// The days that are elpriser.org's forecast, not settled prices.
    pub fn forecast_days(&self) -> Vec<&Day> {
        self.days.iter().filter(|d| d.is_forecast()).collect()
    }

    /// The zone's local date now.
    pub fn local_today(&self) -> NaiveDate {
        self.local_today_at(Utc::now())
    }

    /// The zone's local date at a moment.
    pub fn local_today_at<T: TimeZone>(&self, at: DateTime<T>) -> NaiveDate {
        at.with_timezone(&self.tz()).date_naive()
    }

    // -- current / next -------------------------------------------------------------------

    /// The hour containing `q.at` (default now), priced or not.
    pub fn slot_at(&self, q: &Query) -> Option<Slot> {
        let moment = q.moment();
        self.all_slots(q.include_estimate)
            .into_iter()
            .find(|s| s.start <= moment && moment < s.end)
    }

    /// The priced hour containing now, or `None`.
    ///
    /// `None` means there is no price for this hour (yet), e.g. `gb`, where an hour is only
    /// priced once it is over. See [`now_with`](Series::now_with) for estimates and a fixed
    /// moment. A forecast hour is returned as is ([`Slot::kind`] says so).
    pub fn now(&self) -> Option<Slot> {
        self.now_with(&Query::default())
    }

    /// Like [`now`](Series::now) for `q.at`; with `q.include_estimate` an unpriced hour is
    /// filled with elpriser.org's estimate when the API sent one (kind `Estimate`).
    pub fn now_with(&self, q: &Query) -> Option<Slot> {
        self.slot_at(q).filter(|s| s.price.is_some())
    }

    /// The priced hour that starts at the next hour boundary, or `None`.
    pub fn next_hour(&self) -> Option<Slot> {
        self.next_hour_with(&Query::default())
    }

    /// Like [`next_hour`](Series::next_hour) for `q.at` and `q.include_estimate`.
    pub fn next_hour_with(&self, q: &Query) -> Option<Slot> {
        let moment = floor_hour(q.moment()) + Duration::hours(1);
        self.all_slots(q.include_estimate)
            .into_iter()
            .find(|s| s.start == moment)
            .filter(|s| s.price.is_some())
    }

    /// The newest priced hour that ended no more than three hours ago. For `gb`, whose
    /// current hour is never priced yet.
    pub fn latest_known(&self) -> Option<Slot> {
        self.latest_known_with(&Query::default(), Duration::hours(3))
    }

    /// The newest priced hour that ended no more than `max_age` before `q.at`.
    pub fn latest_known_with(&self, q: &Query, max_age: Duration) -> Option<Slot> {
        let moment = q.moment();
        self.all_slots(false)
            .into_iter()
            .rfind(|s| s.price.is_some() && s.end <= moment && moment - s.end <= max_age)
    }

    // -- days -----------------------------------------------------------------------------

    /// The hours of a local calendar day (empty if the API has none for it).
    pub fn day_slots(&self, when: NaiveDate, include_estimate: bool) -> Vec<Slot> {
        self.all_slots(include_estimate)
            .into_iter()
            .filter(|s| s.local_date() == when)
            .collect()
    }

    /// Today's hours (the zone's local day). Check [`Slot::kind`]: `Forecast` hours are
    /// elpriser.org's own model, not settled prices.
    pub fn today(&self) -> Vec<Slot> {
        self.today_with(&Query::default())
    }

    /// Like [`today`](Series::today) for `q.at` and `q.include_estimate`.
    pub fn today_with(&self, q: &Query) -> Vec<Slot> {
        self.day_slots(self.local_today_at(q.moment()), q.include_estimate)
    }

    /// Tomorrow's hours. Until the day-ahead prices are published (about 13:00 CET) these
    /// are `Forecast` hours (or absent, for `gb` and `xk`); the kind tells which.
    pub fn tomorrow(&self) -> Vec<Slot> {
        self.tomorrow_with(&Query::default())
    }

    /// Like [`tomorrow`](Series::tomorrow) for `q.at` and `q.include_estimate`.
    pub fn tomorrow_with(&self, q: &Query) -> Vec<Slot> {
        let next = self.local_today_at(q.moment()) + Duration::days(1);
        self.day_slots(next, q.include_estimate)
    }

    /// Lowest, highest and mean price of a local day (`None` if nothing is priced).
    pub fn stats(&self, when: NaiveDate) -> Option<DayStats> {
        let priced: Vec<Slot> = self
            .day_slots(when, false)
            .into_iter()
            .filter(|s| s.price.is_some())
            .collect();
        let price = |s: &Slot| s.price.unwrap_or(f64::NAN);
        let minimum = *priced
            .iter()
            .min_by(|a, b| price(a).total_cmp(&price(b)).then(a.start.cmp(&b.start)))?;
        let maximum = *priced
            .iter()
            .max_by(|a, b| price(a).total_cmp(&price(b)).then(b.start.cmp(&a.start)))?;
        let sum: f64 = priced.iter().map(price).sum();
        Some(DayStats {
            minimum,
            maximum,
            average: round5(sum / priced.len() as f64),
            count: priced.len(),
        })
    }

    // -- levels ---------------------------------------------------------------------------

    /// Every hour of today with its [`PriceLevel`].
    pub fn levels(&self) -> Vec<(Slot, PriceLevel)> {
        self.levels_with(&Query::default())
    }

    /// Every hour of the local day `q.day` (default: the day of `q.at`) with its
    /// [`PriceLevel`].
    pub fn levels_with(&self, q: &Query) -> Vec<(Slot, PriceLevel)> {
        let when = q.day.unwrap_or_else(|| self.local_today_at(q.moment()));
        let slots = self.day_slots(when, false);
        let prices: Vec<Option<f64>> = slots.iter().map(|s| s.price).collect();
        slots.into_iter().zip(price_levels(&prices)).collect()
    }

    /// The level of `slot` among the hours of its own local day, `None` if unpriced.
    pub fn level(&self, slot: &Slot) -> Option<PriceLevel> {
        slot.price?;
        self.levels_with(&Query::new().day(slot.local_date()))
            .into_iter()
            .find(|(s, _)| s.start == slot.start)
            .map(|(_, level)| level)
    }

    // -- cheapest hours / best window -----------------------------------------------------

    fn candidates(&self, q: &Query) -> Result<Vec<Slot>> {
        let slots = self.all_slots(q.include_estimate);
        let mut chosen: Vec<Slot> = if let Some(day) = q.day {
            if q.start.is_some() || q.end.is_some() {
                return Err(Error::InvalidArgument(
                    "pass either day or start/end, not both".into(),
                ));
            }
            slots
                .into_iter()
                .filter(|s| s.local_date() == day)
                .collect()
        } else {
            let lo = q.start.unwrap_or_else(|| floor_hour(q.moment()));
            let hi = q.end.unwrap_or(lo + Duration::hours(24));
            slots
                .into_iter()
                .filter(|s| s.start >= lo && s.end <= hi)
                .collect()
        };
        chosen.retain(|s| s.price.is_some());
        if !q.include_forecast {
            chosen.retain(|s| s.kind != PriceKind::Forecast);
        }
        Ok(chosen)
    }

    /// The `n` cheapest priced hours in the next 24 hours, in time order (earliest wins a
    /// tie). Fewer than `n` come back when fewer are priced.
    pub fn cheapest_hours(&self, n: usize) -> Vec<Slot> {
        self.cheapest_hours_with(n, &Query::default())
            .unwrap_or_default()
    }

    /// The `n` cheapest priced hours, in time order (earliest wins a tie).
    ///
    /// Window: the local day `q.day`, or `q.start..q.end`; by default the next 24 hours
    /// counted from the current hour. `settled_only` restricts it to settled prices. Fails
    /// with [`Error::InvalidArgument`] if `day` is combined with `start`/`end`.
    pub fn cheapest_hours_with(&self, n: usize, q: &Query) -> Result<Vec<Slot>> {
        let mut pool = self.candidates(q)?;
        let price = |s: &Slot| s.price.unwrap_or(f64::NAN);
        pool.sort_by(|a, b| price(a).total_cmp(&price(b)).then(a.start.cmp(&b.start)));
        pool.truncate(n);
        pool.sort_by_key(|s| s.start);
        Ok(pool)
    }

    /// The cheapest run of `hours` consecutive priced hours in the next 24 hours (lowest
    /// mean; earliest wins a tie). `None` if no such run exists.
    pub fn best_window(&self, hours: usize) -> Result<Option<Window>> {
        self.best_window_with(hours, &Query::default())
    }

    /// Like [`best_window`](Series::best_window), with the window rules of
    /// [`cheapest_hours_with`](Series::cheapest_hours_with). `hours` must be at least 1.
    pub fn best_window_with(&self, hours: usize, q: &Query) -> Result<Option<Window>> {
        if hours < 1 {
            return Err(Error::InvalidArgument("hours must be >= 1".into()));
        }
        let pool = self.candidates(q)?;
        if pool.len() < hours {
            return Ok(None);
        }
        let mut best: Option<(f64, usize)> = None;
        for i in 0..=(pool.len() - hours) {
            let run = &pool[i..i + hours];
            if run.windows(2).any(|w| w[1].start != w[0].end) {
                continue;
            }
            let total: f64 = run.iter().filter_map(|s| s.price).sum();
            if best.is_none_or(|(b, _)| total < b - 1e-12) {
                best = Some((total, i));
            }
        }
        Ok(best.map(|(total, i)| {
            let run = pool[i..i + hours].to_vec();
            Window {
                start: run[0].start,
                end: run[run.len() - 1].end,
                average: round5(total / hours as f64),
                slots: run,
            }
        }))
    }

    // -- accuracy -------------------------------------------------------------------------

    /// The API's back-test numbers for forecast day `D+horizon` (zone feed), if it sent any.
    pub fn accuracy_for(&self, horizon: u32) -> Option<&Value> {
        self.accuracy
            .as_ref()?
            .get("byHorizon")?
            .get(horizon.to_string())
            .filter(|v| v.is_object())
    }

    /// The API's `low` flag for `horizon` (`None` when it sent no accuracy).
    pub fn is_low_confidence(&self, horizon: u32) -> Option<bool> {
        let entry = self.accuracy_for(horizon)?;
        let low = entry.get("low")?;
        Some(match low {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        })
    }
}

// --------------------------------------------------------------------------
// Danish endpoints (as the API returns them)
// --------------------------------------------------------------------------

/// `/api/now`: this hour's price and whether a schedule strategy says ON.
#[derive(Clone, Debug, PartialEq)]
pub struct DkNow {
    /// The strategy says ON now.
    pub on: bool,
    /// DKK/kWh in the requested [`Mode`].
    pub price: Option<f64>,
    /// The Danish local hour.
    pub hour: u32,
    /// `"DK1"` or `"DK2"`.
    pub area: String,
    /// The price mode.
    pub mode: String,
    /// The strategy.
    pub strategy: String,
}

/// One hour of `/api/prices`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HourPrice {
    /// The Danish local hour (0-23).
    pub hour: u32,
    /// DKK/kWh.
    pub price: Option<f64>,
}

/// `/api/prices` for one date.
#[derive(Clone, Debug, PartialEq)]
pub struct DkPrices {
    /// `"DK1"` or `"DK2"`.
    pub area: String,
    /// The price mode.
    pub mode: String,
    /// The date, `YYYY-MM-DD`.
    pub date: String,
    /// `"DKK/kWh"`.
    pub unit: String,
    /// `"historical"` or `"current"`: which tax/tariff rates a past date is priced with.
    pub charges: Option<String>,
    /// The hourly prices.
    pub prices: Vec<HourPrice>,
    /// The current Danish hour, when the date is today.
    pub current_hour: Option<u32>,
    /// The price of the current hour.
    pub current_price: Option<f64>,
}

/// One hour of `/api/schedule`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduleSlot {
    /// The Danish local hour (0-23).
    pub hour: u32,
    /// DKK/kWh.
    pub price: Option<f64>,
    /// The strategy says ON in this hour.
    pub on: bool,
}

/// `/api/schedule`: a 24 h ON/OFF plan for a strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct DkSchedule {
    /// `"DK1"` or `"DK2"`.
    pub area: String,
    /// The price mode.
    pub mode: String,
    /// The strategy.
    pub strategy: String,
    /// The strategy's `hours` / `pct` parameter as the API applied it.
    pub param: Option<i64>,
    /// The date, `YYYY-MM-DD`.
    pub date: String,
    /// The plan says ON right now.
    pub on_now: bool,
    /// The 24 hours.
    pub schedule: Vec<ScheduleSlot>,
}

impl DkSchedule {
    /// The hours that are ON.
    pub fn on_hours(&self) -> Vec<u32> {
        self.schedule
            .iter()
            .filter(|s| s.on)
            .map(|s| s.hour)
            .collect()
    }
}

/// One hour of `/api/shelly/tariff` (Tibber-compatible).
#[derive(Clone, Debug, PartialEq)]
pub struct TariffPrice {
    /// Price in `currency` per kWh.
    pub total: Option<f64>,
    /// Start of the hour, with the offset the API sent.
    pub starts_at: DateTime<chrono::FixedOffset>,
    /// `"DKK"`.
    pub currency: String,
    /// The API's own level (quintile of the day), e.g. [`PriceLevel::VeryCheap`].
    pub level: Option<PriceLevel>,
}

/// `/api/shelly/tariff`: today and tomorrow.
#[derive(Clone, Debug, PartialEq)]
pub struct TariffDays {
    /// Today's hours.
    pub today: Vec<TariffPrice>,
    /// Tomorrow's hours (empty until they are published).
    pub tomorrow: Vec<TariffPrice>,
}

/// `/api/supplierlookup`: the grid company (netselskab) at an address.
#[derive(Clone, Debug, PartialEq)]
pub struct Supplier {
    /// The address the coordinate resolved to.
    pub address: Option<String>,
    /// The grid company.
    pub name: Option<String>,
    /// The API's own reason when it found nothing, e.g. `"no_address"`.
    pub error: Option<String>,
}

impl Supplier {
    /// A grid company was found.
    pub fn found(&self) -> bool {
        self.name.is_some()
    }
}

// Query for the Danish single-purpose endpoints ------------------------------------------

/// Options of the Danish endpoints ([`Client::dk_now_with`](crate::Client::dk_now_with),
/// `dk_prices_with`, `dk_schedule_with`, `shelly_tariff_with`).
///
/// Which fields each endpoint takes:
///
/// | endpoint | fields |
/// |---|---|
/// | `dk_now` | `mode`, `gln`, `strategy`, `hours`, `pct`, `max_off` |
/// | `dk_schedule` | `mode`, `gln`, `strategy`, `hours`, `pct`, `max_off`, `date` |
/// | `dk_prices` | `mode`, `gln`, `date`, `charges` |
/// | `shelly_tariff` | `mode`, `gln` |
///
/// A field the endpoint does not take is an [`Error::InvalidArgument`], and so are values out
/// of range (`hours` 1-23, `pct` 1-100, `max_off` 1-12) or a `net_` mode without a GLN, all
/// before any request is made.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct DkQuery {
    /// The price mode (default [`Mode::InklAlt`]).
    pub mode: Option<Mode>,
    /// The grid company's GLN, needed by the `net_` modes.
    pub gln: Option<String>,
    /// The schedule strategy.
    pub strategy: Option<Strategy>,
    /// Strategy parameter: how many hours (1-23).
    pub hours: Option<u32>,
    /// Strategy parameter: percent of the day (1-100).
    pub pct: Option<u32>,
    /// Strategy parameter: longest run of OFF hours (1-12).
    pub max_off: Option<u32>,
    /// The Danish date (default today).
    pub date: Option<NaiveDate>,
    /// Which rates a past date is priced with.
    pub charges: Option<Charges>,
}

impl DkQuery {
    /// An empty query: the API's defaults.
    pub fn new() -> DkQuery {
        DkQuery::default()
    }

    /// Pick the price mode.
    pub fn mode(mut self, mode: Mode) -> DkQuery {
        self.mode = Some(mode);
        self
    }

    /// The grid company's GLN.
    pub fn gln(mut self, gln: impl Into<String>) -> DkQuery {
        self.gln = Some(gln.into());
        self
    }

    /// The schedule strategy.
    pub fn strategy(mut self, strategy: Strategy) -> DkQuery {
        self.strategy = Some(strategy);
        self
    }

    /// Strategy parameter: how many hours (1-23).
    pub fn hours(mut self, hours: u32) -> DkQuery {
        self.hours = Some(hours);
        self
    }

    /// Strategy parameter: percent of the day (1-100).
    pub fn pct(mut self, pct: u32) -> DkQuery {
        self.pct = Some(pct);
        self
    }

    /// Strategy parameter: longest run of OFF hours (1-12).
    pub fn max_off(mut self, max_off: u32) -> DkQuery {
        self.max_off = Some(max_off);
        self
    }

    /// The Danish date.
    pub fn date(mut self, date: NaiveDate) -> DkQuery {
        self.date = Some(date);
        self
    }

    /// Which rates a past date is priced with.
    pub fn charges(mut self, charges: Charges) -> DkQuery {
        self.charges = Some(charges);
        self
    }
}
