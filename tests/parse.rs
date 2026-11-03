//! Parsing of recorded API answers (tests/fixtures, recorded 2026-11-03 from the live API).
mod common;

use chrono::{Duration, NaiveDate, TimeZone, Timelike, Utc};
use common::{assert_close, load};
use elpriser::parse;
use elpriser::{Error, PriceKind, PriceLevel, Series, Zone};
use serde_json::json;

fn series(zone: Zone) -> Series {
    let name = format!("zone_{}", zone.key().replace('-', "_"));
    parse::parse_zone_feed(&load(&name), zone).unwrap()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[test]
fn euro_zone_basics() {
    let s = series(Zone::De);
    assert_eq!(
        (s.currency.as_str(), s.unit.as_str(), s.rate),
        ("EUR", "EUR/MWh", Some(1.0))
    );
    assert_eq!(s.rate_source.as_deref(), Some("ECB"));
    assert_eq!(s.source, elpriser::SeriesSource::ZoneFeed);
    assert_eq!(s.price_unit(), "EUR/kWh");
    assert_eq!(s.days[0].date, d(2026, 11, 1));
    assert!(s.days[..3].iter().all(|day| day.kind == PriceKind::Actual));
    let horizons: Vec<_> = s.forecast_days().iter().map(|day| day.horizon).collect();
    assert_eq!(horizons, [2, 3, 4, 5, 6, 7].map(Some));
    assert!(s.days.iter().all(|day| day.slots.len() == 24));
}

#[test]
fn price_is_mwh_times_rate_over_1000() {
    let s = series(Zone::De);
    let slot = s.day(d(2026, 11, 3)).unwrap().slots[8];
    assert_eq!(slot.price_mwh, Some(301.31));
    assert_close(slot.price.unwrap(), 0.30131, 1e-12);
    assert_eq!(slot.hour(), 8);
    assert_eq!(slot.kind, PriceKind::Actual);
    assert!(slot.min.is_none() && slot.max.is_none());
}

#[test]
fn forecast_hours_carry_p10_p90() {
    let s = series(Zone::De);
    let slot = s.day(d(2026, 11, 4)).unwrap().slots[0];
    assert_eq!(slot.kind, PriceKind::Forecast);
    assert_eq!(
        (slot.price_mwh, slot.min_mwh, slot.max_mwh),
        (Some(197.16), Some(161.45), Some(276.16))
    );
    assert_close(slot.min.unwrap(), 0.16145, 1e-12);
    assert_close(slot.price.unwrap(), 0.19716, 1e-12);
    assert_close(slot.max.unwrap(), 0.27616, 1e-12);
}

#[test]
fn local_currency_conversion_uses_the_ecb_rate() {
    let pl = series(Zone::Pl);
    assert_eq!((pl.currency.as_str(), pl.unit.as_str()), ("PLN", "EUR/MWh"));
    assert_eq!(
        (pl.rate, pl.rate_date.as_deref()),
        (Some(4.4185), Some("2026-11-02"))
    );
    let raw = load("zone_pl")["days"][2]["prices"][12]["eur_mwh"]
        .as_f64()
        .unwrap();
    assert_close(
        pl.day(d(2026, 11, 3)).unwrap().slots[12].price.unwrap(),
        raw * 4.4185 / 1000.0,
        1e-5,
    );
    let se3 = series(Zone::Se3);
    assert_eq!((se3.currency.as_str(), se3.rate), ("SEK", Some(10.9475)));
    let slot = se3.day(d(2026, 11, 3)).unwrap().slots[3];
    assert!(slot.price.unwrap() > slot.price_mwh.unwrap() / 1000.0);
}

#[test]
fn zone_tz_is_applied_not_utc() {
    let s = series(Zone::De);
    let slot = s.day(d(2026, 11, 3)).unwrap().slots[8];
    assert_eq!(
        slot.start,
        Utc.with_ymd_and_hms(2026, 11, 3, 7, 0, 0).unwrap()
    );
    assert_eq!(slot.end - slot.start, Duration::hours(1));
    assert_eq!(
        (
            slot.local_start().hour(),
            slot.local_start().offset().to_string()
        ),
        (8, "CET".into())
    );
    let pt = series(Zone::Pt);
    assert_eq!(
        pt.day(d(2026, 11, 3)).unwrap().slots[8].start,
        Utc.with_ymd_and_hms(2026, 11, 3, 8, 0, 0).unwrap()
    );
}

#[test]
fn accuracy_passthrough() {
    let s = series(Zone::De);
    assert_eq!(s.model, Some(json!("eu-ens-v2")));
    assert_eq!(s.forecast_generated.as_deref(), Some("2026-11-03"));
    assert_eq!(s.trained_through.as_deref(), Some("2026-11-01"));
    assert_eq!(s.accuracy_for(2).unwrap()["mae"], json!(19.7));
    assert_eq!(s.is_low_confidence(2), Some(false));
    assert!(s.accuracy_for(1).is_none());
    assert_eq!(s.is_low_confidence(1), None);
    assert_eq!(series(Zone::Pt).is_low_confidence(2), Some(true));
}

#[test]
fn zone_without_forecast() {
    let s = series(Zone::Xk);
    assert!(s.forecast_days().is_empty());
    assert!(s.accuracy.is_none() && s.trained_through.is_none());
    assert_eq!(s.days.len(), 3);
}

#[test]
fn nordic_zone_forecast_has_no_horizon() {
    let s = series(Zone::Se3);
    assert_eq!(s.forecast_days().len(), 8);
    assert!(s.forecast_days().iter().all(|day| day.horizon.is_none()));
    assert!(s.model.is_none());
}

#[test]
fn gb_is_published_in_pounds_despite_the_field_name() {
    let s = series(Zone::Gb);
    assert_eq!(
        (s.unit.as_str(), s.currency.as_str(), s.rate),
        ("GBP/MWh", "GBP", Some(1.0))
    );
    let first = s.day(d(2026, 11, 2)).unwrap().slots[0];
    assert_eq!(first.price_mwh, Some(191.1));
    assert_close(first.price.unwrap(), 0.1911, 1e-12);
    assert_eq!(s.price_unit(), "GBP/kWh");
}

#[test]
fn gb_today_has_unpriced_hours_and_an_estimate() {
    let s = series(Zone::Gb);
    let today = s.day(d(2026, 11, 3)).unwrap();
    let unpriced: Vec<usize> = today
        .slots
        .iter()
        .enumerate()
        .filter(|(_, sl)| sl.price.is_none())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(unpriced, (7..24).collect::<Vec<_>>());
    assert!(today.slots.iter().all(|sl| sl.kind == PriceKind::Actual));
    let est = s.estimate.as_ref().unwrap();
    assert_eq!(
        (est.date, est.scope.as_str(), est.model.as_deref()),
        (d(2026, 11, 3), "partial-day", Some("gb-est-v1"))
    );
    assert_eq!(
        est.generated_at,
        Some(Utc.with_ymd_and_hms(2026, 11, 3, 7, 12, 1).unwrap())
    );
    let hours: Vec<u32> = est.slots.iter().map(|sl| sl.hour()).collect();
    assert_eq!(hours, (7..24).collect::<Vec<_>>());
    assert!(est
        .slots
        .iter()
        .all(|sl| sl.kind == PriceKind::Estimate && sl.min.is_some() && sl.max.is_some()));
    // days is never changed by the estimate
    assert!(today.slots[7].price.is_none());
    let horizons: Vec<_> = s.forecast_days().iter().map(|day| day.horizon).collect();
    assert_eq!(horizons, [1, 2, 3, 4, 5, 6, 7].map(Some));
}

#[test]
fn pt_ie_estimate_is_hour_23_of_the_newest_day() {
    for zone in [Zone::Pt, Zone::Ie] {
        let s = series(zone);
        let est = s.estimate.as_ref().unwrap();
        assert_eq!(est.model.as_deref(), Some("hour23-v1"), "{zone}");
        assert_eq!(
            est.slots.iter().map(|sl| sl.hour()).collect::<Vec<_>>(),
            [23]
        );
        let method = est.method.as_deref().unwrap();
        assert!(["last-settled-hour", "last-settled-hour-plus-typical-step"].contains(&method));
        assert_eq!(est.accuracy.as_ref().unwrap()["hours"], json!(1));
        assert!(s.day(est.date).unwrap().slots[23].price.is_none());
    }
}

#[test]
fn zones_without_estimate() {
    assert!(series(Zone::De).estimate.is_none());
}

#[test]
fn dk_forecast_feed() {
    let s = parse::parse_dk_forecast(&load("dk_forecast"), Zone::Dk1, "inkl_alt", None).unwrap();
    assert_eq!(
        (
            s.currency.as_str(),
            s.unit.as_str(),
            s.mode.as_deref(),
            s.rate
        ),
        ("DKK", "DKK/kWh", Some("inkl_alt"), None)
    );
    assert_eq!(s.source, elpriser::SeriesSource::DkForecast);
    assert_eq!(
        s.charges.as_ref().unwrap(),
        &json!({"sys": 0.072, "trans": 0.043, "afg": 0.008, "vat": 0.25})
    );
    assert_eq!(s.model.as_ref().unwrap()["name"], json!("v3"));
    assert_eq!(
        s.forecast_generated.as_deref(),
        Some("2026-11-02T12:13:57.958767+00:00")
    );
    assert_eq!(
        [s.days[0].kind, s.days[1].kind],
        [PriceKind::Actual, PriceKind::Forecast]
    );
    assert_eq!(s.days[1].source.as_deref(), Some("model"));
    assert_eq!(s.days.last().unwrap().source.as_deref(), Some("heuristic"));
    assert_eq!(s.days[0].weekday, Some(2));
    for day in &s.days {
        use chrono::Datelike;
        assert_eq!(
            day.weekday,
            Some(i64::from(day.date.weekday().num_days_from_sunday()))
        );
    }
    let slot = s.days[1].slots[0];
    let raw = &load("dk_forecast")["days"][1]["prices"][0];
    assert_eq!(slot.price, raw["price"].as_f64());
    assert_eq!(slot.min, raw["min"].as_f64());
    assert_eq!(slot.max, raw["max"].as_f64());
    assert_eq!(slot.price_mwh, None);
    assert!(s.days[0].slots.iter().all(|sl| sl.min.is_none()));
}

#[test]
fn dk_net_mode_carries_grid_tariff() {
    let s = parse::parse_dk_forecast(
        &load("dk_forecast_net"),
        Zone::Dk2,
        "net_inkl_alt",
        Some("5790000704842"),
    )
    .unwrap();
    assert_eq!(
        (s.mode.as_deref(), s.gln.as_deref()),
        (Some("net_inkl_alt"), Some("5790000704842"))
    );
    let day = &s.days[0];
    let grid = day.grid_tariff.as_ref().unwrap();
    assert_eq!(
        (day.tariff.as_deref(), grid.len(), grid[17]),
        (Some("published"), 24, Some(0.5449))
    );
}

#[test]
fn dk_forecast_rejects_a_non_danish_zone() {
    let err =
        parse::parse_dk_forecast(&load("dk_forecast"), Zone::De, "inkl_alt", None).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)));
}

#[test]
fn zone_feed_mismatch_is_rejected() {
    let err = parse::parse_zone_feed(&load("zone_de"), Zone::Fr).unwrap_err();
    assert!(
        matches!(&err, Error::Parse(m) if m.contains("answered")),
        "{err}"
    );
}

#[test]
fn malformed_zone_feed() {
    let bad = [
        json!([]),
        json!("x"),
        json!({}),
        json!({"zoneInfo": {"currency": "EUR", "rate": 1}}),
        json!({"zoneInfo": {"currency": "EUR", "rate": 1}, "days": []}),
        json!({"zoneInfo": {"currency": "EUR", "rate": 0}, "days": [{"date": "2026-11-03", "prices": []}]}),
        json!({"zoneInfo": {"currency": "EU", "rate": 1}, "days": [{"date": "2026-11-03", "prices": []}]}),
        json!({"zoneInfo": {"currency": "EUR", "rate": 1}, "days": [{"date": "garbage", "prices": []}]}),
    ];
    for payload in bad {
        let err = parse::parse_zone_feed(&payload, Zone::De).unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "{payload}");
    }
}

#[test]
fn malformed_hours_become_unpriced_hours() {
    let payload = json!({
        "zoneInfo": {"currency": "EUR", "rate": 1},
        "days": [{
            "date": "2026-11-03",
            "type": "actual",
            "prices": [
                {"hour": 0, "eur_mwh": "12"},
                {"hour": 1, "eur_mwh": true},
                {"hour": 2, "eur_mwh": null},
                {"hour": 3, "eur_mwh": 5},
                {"hour": 99, "eur_mwh": 1},
                "junk",
                {"eur_mwh": 1},
            ],
        }],
    });
    let s = parse::parse_zone_feed(&payload, Zone::De).unwrap();
    let prices: Vec<_> = s.days[0].slots.iter().map(|sl| sl.price).collect();
    assert_eq!(prices, [None, None, None, Some(0.005)]);
}

#[test]
fn unknown_fields_are_ignored() {
    let mut payload = load("zone_de");
    payload["brand_new_field"] = json!({"x": 1});
    payload["days"][0]["extra"] = json!(1);
    let s = parse::parse_zone_feed(&payload, Zone::De).unwrap();
    assert!(s.days[0].slots[0].price.is_some());
    assert_eq!(s.raw["brand_new_field"], json!({"x": 1}));
}

#[test]
fn dk_endpoints() {
    let now = parse::parse_dk_now(&load("dk_now")).unwrap();
    assert_eq!(
        (
            now.on,
            now.price,
            now.hour,
            now.area.as_str(),
            now.mode.as_str(),
            now.strategy.as_str()
        ),
        (false, Some(2.9896), 8, "DK1", "inkl_alt", "cheapest_n")
    );
    let prices = parse::parse_dk_prices(&load("dk_prices")).unwrap();
    assert_eq!(
        (
            prices.unit.as_str(),
            prices.charges.as_deref(),
            prices.current_hour,
            prices.current_price
        ),
        ("DKK/kWh", Some("historical"), Some(8), Some(2.9896))
    );
    assert_eq!(prices.prices.len(), 24);
    assert_eq!(prices.prices[8].price, Some(2.9896));
    let sched = parse::parse_dk_schedule(&load("dk_schedule")).unwrap();
    assert_eq!(
        (sched.strategy.as_str(), sched.param, sched.on_now),
        ("cheapest_n", Some(6), false)
    );
    assert_eq!(sched.on_hours(), [0, 1, 2, 3, 4, 5]);
    assert_eq!(sched.schedule.len(), 24);
}

#[test]
fn dk_endpoint_shapes_are_checked() {
    assert!(parse::parse_dk_now(&json!({"price": 1})).is_err());
    assert!(parse::parse_dk_now(&json!({"hour": "8"})).is_err());
    assert!(parse::parse_dk_now(&json!([])).is_err());
    assert!(parse::parse_dk_prices(&json!({"area": "DK1"})).is_err());
    assert!(parse::parse_dk_schedule(&json!({"schedule": 1})).is_err());
    assert!(parse::parse_shelly_tariff(&json!({"data": {}})).is_err());
}

#[test]
fn shelly_tariff_and_levels_match_client_side_calculation() {
    let tariff = parse::parse_shelly_tariff(&load("dk_shelly")).unwrap();
    assert_eq!(tariff.today.len(), 24);
    assert!(tariff.tomorrow.is_empty());
    let first = &tariff.today[0];
    assert_eq!(
        (first.currency.as_str(), first.level),
        ("DKK", Some(PriceLevel::VeryCheap))
    );
    assert_eq!(first.starts_at.offset().local_minus_utc(), 3600);
    // our quintile algorithm reproduces the API's own levels exactly
    let totals: Vec<_> = tariff.today.iter().map(|t| t.total).collect();
    let mine = elpriser::price_levels(&totals);
    let api: Vec<_> = tariff.today.iter().map(|t| t.level.unwrap()).collect();
    assert_eq!(mine, api);
}

#[test]
fn supplier() {
    let ok = parse::parse_supplier(&load("supplier_ok")).unwrap();
    assert!(ok.found() && ok.name.is_some() && ok.address.is_some() && ok.error.is_none());
    let none = parse::parse_supplier(&load("supplier_none")).unwrap();
    assert!(!none.found());
    assert_eq!(none.error.as_deref(), Some("no_address"));
}

#[test]
fn local_day_starts_follow_daylight_saving() {
    let tz = Zone::Dk1.tz();
    assert_eq!(parse::local_day_starts(d(2026, 10, 25), tz).len(), 25);
    assert_eq!(parse::local_day_starts(d(2026, 3, 29), tz).len(), 23);
    assert_eq!(parse::local_day_starts(d(2026, 6, 10), tz).len(), 24);
}
