//! Turn raw API JSON into the result types ([`Series`], [`DkNow`] ...). Pure functions, no I/O.
//!
//! The parsers are strict about the structure (a missing `days` list or `zoneInfo` is an
//! [`Error::Parse`]) and lenient about everything else: unknown fields are ignored, and a
//! malformed hour becomes an unpriced hour rather than an error.
//!
//! You normally do not call these yourself; they are public so a recorded answer can be
//! replayed offline (see also [`Series::from_zone_feed_json`](crate::Series::from_zone_feed_json)).

use std::collections::{BTreeMap, HashMap};

use chrono::{
    DateTime, Duration, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Timelike, Utc,
};
use chrono_tz::Tz;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::models::{
    round5, Day, DkNow, DkPrices, DkSchedule, Estimate, HourPrice, PriceKind, PriceLevel,
    ScheduleSlot, Series, SeriesSource, Slot, Supplier, TariffDays, TariffPrice,
};
use crate::zones::Zone;

fn parse_err(message: impl Into<String>) -> Error {
    Error::Parse(message.into())
}

/// A finite number or `None`. Booleans and strings are not prices.
fn num(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// An integer (not a float, not a boolean).
fn int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn hour_of_day(value: Option<&Value>) -> Option<u32> {
    int(value).and_then(|i| u32::try_from(i).ok())
}

fn need_object<'a>(payload: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>> {
    payload
        .as_object()
        .ok_or_else(|| parse_err(format!("{what}: response is not a JSON object")))
}

fn parse_date(value: Option<&Value>) -> Option<NaiveDate> {
    match value {
        Some(Value::String(s)) => NaiveDate::parse_from_str(s, "%Y-%m-%d").ok(),
        _ => None,
    }
}

fn parse_dt(value: Option<&Value>) -> Option<DateTime<FixedOffset>> {
    let text = match value {
        Some(Value::String(s)) if !s.is_empty() => s,
        _ => return None,
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt);
    }
    // No offset: the API means UTC (like the Python SDK).
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .ok()?;
    Some(naive.and_utc().fixed_offset())
}

/// UTC start of every hour of the local calendar day (23, 24 or 25 of them).
pub fn local_day_starts(day: NaiveDate, tz: Tz) -> Vec<DateTime<Utc>> {
    let midnight = |d: NaiveDate| -> DateTime<Utc> {
        let naive = d.and_time(NaiveTime::MIN);
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
            // None of the zones has a midnight gap; read it as UTC rather than panic.
            LocalResult::None => naive.and_utc(),
        }
    };
    let start = midnight(day);
    let end = midnight(day + Duration::days(1));
    let count = (end - start).num_hours().max(0);
    (0..count).map(|i| start + Duration::hours(i)).collect()
}

struct Keys {
    price: &'static str,
    min: &'static str,
    max: &'static str,
    scale: f64,
    mwh: bool,
}

const ZONE_FEED_KEYS: Keys = Keys {
    price: "eur_mwh",
    min: "min_eur_mwh",
    max: "max_eur_mwh",
    scale: 1.0, // replaced by rate / 1000
    mwh: true,
};

/// One API day (rows keyed by local hour) as UTC-anchored hours.
///
/// Daylight saving: on a 25-hour day the API carries one value for the doubled local hour 2,
/// and both 02:00 hours get it; on a 23-hour day there is no hour 2 and no slot. A row with a
/// null price still yields a slot (`price: None`) so the day keeps its shape.
fn expand_day(day: NaiveDate, tz: Tz, rows: &[Value], keys: &Keys, kind: PriceKind) -> Vec<Slot> {
    let mut by_hour: HashMap<u32, &Value> = HashMap::new();
    for row in rows {
        if let Some(hour) = hour_of_day(row.get("hour")).filter(|h| *h <= 23) {
            by_hour.insert(hour, row);
        }
    }
    let scaled = |v: Option<f64>| v.map(|v| round5(v * keys.scale));
    let mut slots = Vec::new();
    for start in local_day_starts(day, tz) {
        let Some(row) = by_hour.get(&start.with_timezone(&tz).hour()) else {
            continue;
        };
        let value = num(row.get(keys.price));
        let low = value.and(num(row.get(keys.min)));
        let high = value.and(num(row.get(keys.max)));
        slots.push(Slot {
            start,
            end: start + Duration::hours(1),
            price: scaled(value),
            kind,
            min: scaled(low),
            max: scaled(high),
            price_mwh: if keys.mwh { value } else { None },
            min_mwh: if keys.mwh { low } else { None },
            max_mwh: if keys.mwh { high } else { None },
            tz,
        });
    }
    slots
}

fn parse_days(payload: &serde_json::Map<String, Value>, tz: Tz, keys: &Keys) -> Result<Vec<Day>> {
    let days = match payload.get("days") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Err(parse_err("response has no days")),
    };
    let mut out: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    for entry in days {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(date) = parse_date(entry.get("date")) else {
            continue;
        };
        let Some(Value::Array(rows)) = entry.get("prices") else {
            continue;
        };
        let kind = if entry.get("type").and_then(Value::as_str) == Some("forecast") {
            PriceKind::Forecast
        } else {
            PriceKind::Actual
        };
        let grid_tariff = match entry.get("grid_tariff") {
            Some(Value::Array(a)) => Some(a.iter().map(|v| num(Some(v))).collect()),
            _ => None,
        };
        out.insert(
            date,
            Day {
                date,
                kind,
                slots: expand_day(date, tz, rows, keys, kind),
                horizon: int(entry.get("horizon")),
                weekday: int(entry.get("weekday")),
                source: string(entry.get("source")),
                tariff: string(entry.get("tariff")),
                grid_tariff,
            },
        );
    }
    if out.is_empty() {
        return Err(parse_err("response has no usable days"));
    }
    Ok(out.into_values().collect())
}

fn parse_estimate(raw: Option<&Value>, tz: Tz, scale: f64) -> Option<Estimate> {
    let raw = raw?.as_object()?;
    let date = parse_date(raw.get("date"))?;
    let Some(Value::Array(rows)) = raw.get("prices") else {
        return None;
    };
    let keys = Keys {
        scale,
        ..ZONE_FEED_KEYS
    };
    let slots = expand_day(date, tz, rows, &keys, PriceKind::Estimate);
    Some(Estimate {
        date,
        scope: string(raw.get("scope")).unwrap_or_else(|| "partial-day".into()),
        model: string(raw.get("model")),
        generated_at: parse_dt(raw.get("generatedAt")).map(|d| d.with_timezone(&Utc)),
        slots,
        method: string(raw.get("method")),
        accuracy: raw.get("accuracy").filter(|v| v.is_object()).cloned(),
    })
}

/// `GET /api/nordic?zone=<zone>`: hourly `eur_mwh` plus `zoneInfo` with the ECB rate.
///
/// Despite the field name `eur_mwh` the unit is whatever `unit` says (`GBP/MWh` for `gb`);
/// local currency per kWh is `value * zoneInfo.rate / 1000` (`rate` is 1 for `gb`).
pub fn parse_zone_feed(payload: &Value, zone: Zone) -> Result<Series> {
    let data = need_object(payload, "zone feed")?;
    if let Some(Value::String(echoed)) = data.get("zone") {
        if echoed.to_lowercase() != zone.key() {
            return Err(parse_err(format!(
                "asked for zone {:?} but the API answered {:?}",
                zone.key(),
                echoed
            )));
        }
    }
    let info = data
        .get("zoneInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| parse_err("response has no zoneInfo"))?;
    let currency = match info.get("currency") {
        Some(Value::String(c)) if c.chars().count() == 3 => c.to_uppercase(),
        _ => return Err(parse_err("zoneInfo has no currency")),
    };
    let rate = match num(info.get("rate")) {
        Some(r) if r > 0.0 => r,
        _ => return Err(parse_err("zoneInfo has no usable rate")),
    };
    let tz = zone.tz();
    let scale = rate / 1000.0;
    let keys = Keys {
        scale,
        ..ZONE_FEED_KEYS
    };
    let days = parse_days(data, tz, &keys)?;
    Ok(Series {
        zone,
        currency,
        unit: string(data.get("unit")).unwrap_or_else(|| "EUR/MWh".into()),
        days,
        estimate: parse_estimate(data.get("estimate"), tz, scale),
        source: SeriesSource::ZoneFeed,
        mode: None,
        gln: None,
        rate: Some(rate),
        rate_date: string(info.get("rateDate")),
        rate_source: string(info.get("rateSource")),
        generated: string(data.get("generated")),
        trained_through: string(data.get("trainedThrough")),
        model: string(data.get("model")).map(Value::String),
        forecast_generated: string(data.get("forecastGenerated")),
        accuracy: data.get("accuracy").filter(|v| v.is_object()).cloned(),
        charges: None,
        raw: payload.clone(),
    })
}

/// `GET /api/forecast?area=DK1|DK2&mode=...`: DKK/kWh in the requested mode, today + 9 days.
pub fn parse_dk_forecast(
    payload: &Value,
    zone: Zone,
    mode: &str,
    gln: Option<&str>,
) -> Result<Series> {
    let data = need_object(payload, "forecast")?;
    if zone.area().is_none() {
        return Err(Error::InvalidArgument(format!(
            "{} is not a Danish zone",
            zone.key()
        )));
    }
    let tz = zone.tz();
    let keys = Keys {
        price: "price",
        min: "min",
        max: "max",
        scale: 1.0,
        mwh: false,
    };
    let days = parse_days(data, tz, &keys)?;
    let model = data.get("model").filter(|v| v.is_object()).cloned();
    let forecast_generated = model.as_ref().and_then(|m| string(m.get("generatedAt")));
    Ok(Series {
        zone,
        currency: "DKK".into(),
        unit: "DKK/kWh".into(),
        days,
        estimate: None,
        source: SeriesSource::DkForecast,
        mode: Some(string(data.get("mode")).unwrap_or_else(|| mode.to_string())),
        gln: gln.map(str::to_string),
        rate: None,
        rate_date: None,
        rate_source: None,
        generated: string(data.get("generated")),
        trained_through: None,
        model,
        forecast_generated,
        accuracy: None,
        charges: data.get("charges").filter(|v| v.is_object()).cloned(),
        raw: payload.clone(),
    })
}

// -- Danish single-purpose endpoints ----------------------------------------------------

/// Python's `str(data.get(key))` for a text field: the text, or empty when absent.
fn text(data: &serde_json::Map<String, Value>, key: &str) -> String {
    match data.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `GET /api/now`.
pub fn parse_dk_now(payload: &Value) -> Result<DkNow> {
    let data = need_object(payload, "now")?;
    if !data.contains_key("hour") {
        return Err(parse_err("now: missing \"hour\""));
    }
    let hour =
        hour_of_day(data.get("hour")).ok_or_else(|| parse_err("now: hour is not an integer"))?;
    Ok(DkNow {
        on: truthy(data.get("on")),
        price: num(data.get("price")),
        hour,
        area: text(data, "area"),
        mode: text(data, "mode"),
        strategy: text(data, "strategy"),
    })
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn hour_prices(rows: Option<&Value>, what: &str) -> Result<Vec<HourPrice>> {
    let Some(Value::Array(rows)) = rows else {
        return Err(parse_err(format!("{what}: prices is not a list")));
    };
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(HourPrice {
                hour: hour_of_day(row.get("hour"))?,
                price: num(row.get("price")),
            })
        })
        .collect())
}

/// `GET /api/prices`.
pub fn parse_dk_prices(payload: &Value) -> Result<DkPrices> {
    let data = need_object(payload, "prices")?;
    Ok(DkPrices {
        area: text(data, "area"),
        mode: text(data, "mode"),
        date: text(data, "date"),
        unit: string(data.get("unit")).unwrap_or_else(|| "DKK/kWh".into()),
        charges: string(data.get("charges")),
        prices: hour_prices(data.get("prices"), "prices")?,
        current_hour: hour_of_day(data.get("current_hour")),
        current_price: num(data.get("current_price")),
    })
}

/// `GET /api/schedule`.
pub fn parse_dk_schedule(payload: &Value) -> Result<DkSchedule> {
    let data = need_object(payload, "schedule")?;
    let Some(Value::Array(rows)) = data.get("schedule") else {
        return Err(parse_err("schedule: schedule is not a list"));
    };
    let schedule = rows
        .iter()
        .filter_map(|row| {
            Some(ScheduleSlot {
                hour: hour_of_day(row.get("hour"))?,
                price: num(row.get("price")),
                on: truthy(row.get("on")),
            })
        })
        .collect();
    Ok(DkSchedule {
        area: text(data, "area"),
        mode: text(data, "mode"),
        strategy: text(data, "strategy"),
        param: int(data.get("param")),
        date: text(data, "date"),
        on_now: truthy(data.get("on_now")),
        schedule,
    })
}

/// `GET /api/shelly/tariff`: the Tibber-shaped answer, `today` and `tomorrow`.
pub fn parse_shelly_tariff(payload: &Value) -> Result<TariffDays> {
    need_object(payload, "shelly tariff")?;
    let info = payload
        .pointer("/data/viewer/homes/0/currentSubscription/priceInfo")
        .ok_or_else(|| parse_err("shelly tariff: unexpected shape"))?;
    let day = |rows: Option<&Value>| -> Vec<TariffPrice> {
        let Some(Value::Array(rows)) = rows else {
            return Vec::new();
        };
        rows.iter()
            .filter_map(|row| {
                row.as_object()?;
                Some(TariffPrice {
                    total: num(row.get("total")),
                    starts_at: parse_dt(row.get("startsAt"))?,
                    currency: string(row.get("currency")).unwrap_or_else(|| "DKK".into()),
                    level: row
                        .get("level")
                        .and_then(Value::as_str)
                        .and_then(|l| l.parse::<PriceLevel>().ok()),
                })
            })
            .collect()
    };
    Ok(TariffDays {
        today: day(info.get("today")),
        tomorrow: day(info.get("tomorrow")),
    })
}

/// `GET /api/supplierlookup`.
pub fn parse_supplier(payload: &Value) -> Result<Supplier> {
    let data = need_object(payload, "supplier lookup")?;
    Ok(Supplier {
        address: string(data.get("address")),
        name: string(data.get("name")),
        error: string(data.get("error")),
    })
}
