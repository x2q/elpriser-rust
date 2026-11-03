//! The questions a Series answers: now/next/today/tomorrow, cheapest hours, windows, levels.
mod common;

use chrono::{Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use common::{assert_close, at, day_json, load, some, synthetic};
use elpriser::{price_levels, Error, PriceKind, PriceLevel, Query, Series, Zone};
use serde_json::{json, Value};

fn zone_series(zone: Zone) -> Series {
    let name = format!("zone_{}", zone.key().replace('-', "_"));
    Series::from_zone_feed_json(zone, &load(&name)).unwrap()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn utc(y: i32, m: u32, day: u32, h: u32, min: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, day, h, min, 0).unwrap()
}

fn q_at(moment: chrono::DateTime<Utc>) -> Query {
    Query::new().at(moment)
}

fn hours(slots: &[elpriser::Slot]) -> Vec<u32> {
    slots.iter().map(|s| s.hour()).collect()
}

const JUN10: &str = "2026-06-10";

// -- current / next ---------------------------------------------------------------------------

#[test]
fn now_and_next_hour() {
    let s = zone_series(Zone::De);
    let q = q_at(at());
    let now = s.now_with(&q).unwrap();
    assert_eq!((now.hour(), now.price_mwh), (8, Some(301.31)));
    let next = s.next_hour_with(&q).unwrap();
    assert_eq!((next.hour(), next.price_mwh), (9, Some(270.94)));
    assert_eq!(now.kind, PriceKind::Actual);
}

#[test]
fn now_accepts_other_offsets() {
    let s = zone_series(Zone::De);
    let elsewhere = FixedOffset::west_opt(5 * 3600)
        .unwrap()
        .from_utc_datetime(&at().naive_utc());
    assert_eq!(
        s.now_with(&Query::new().at(elsewhere)),
        s.now_with(&q_at(at()))
    );
}

#[test]
fn now_none_outside_the_data() {
    let s = zone_series(Zone::De);
    let q = q_at(utc(2030, 1, 1, 0, 0));
    assert!(s.now_with(&q).is_none());
    assert!(s.next_hour_with(&q).is_none());
}

#[test]
fn now_uses_the_real_clock_by_default() {
    // The recorded answer is from 2026-11-03; whatever the date is now, these must not panic.
    let s = zone_series(Zone::De);
    let _ = (
        s.now(),
        s.next_hour(),
        s.today(),
        s.tomorrow(),
        s.levels(),
        s.latest_known(),
    );
    assert_eq!(s.local_today(), s.local_today_at(Utc::now()));
}

#[test]
fn next_hour_crosses_midnight_into_the_forecast_day() {
    let s = zone_series(Zone::De);
    let next = s.next_hour_with(&q_at(utc(2026, 11, 3, 22, 30))).unwrap(); // 23:30 Berlin
    assert_eq!(
        (next.local_date(), next.hour(), next.kind),
        (d(2026, 11, 4), 0, PriceKind::Forecast)
    );
}

#[test]
fn gb_current_hour_is_unpriced_until_estimated() {
    let s = zone_series(Zone::Gb);
    assert!(s.slot_at(&q_at(at())).unwrap().price.is_none());
    assert!(s.now_with(&q_at(at())).is_none());
    let est = s.now_with(&q_at(at()).with_estimate()).unwrap();
    assert_eq!(
        (est.kind, est.hour(), est.price_mwh),
        (PriceKind::Estimate, 7, Some(209.85))
    );
    assert!(est.min.unwrap() < est.price.unwrap() && est.price.unwrap() < est.max.unwrap());
    assert_close(est.price.unwrap(), 0.20985, 1e-12);
}

#[test]
fn gb_latest_known_falls_back_to_the_last_settled_hour() {
    let s = zone_series(Zone::Gb);
    let last = s
        .latest_known_with(&q_at(at()), Duration::hours(3))
        .unwrap();
    assert_eq!((last.hour(), last.price_mwh), (6, Some(202.06)));
    let later = q_at(at() + Duration::hours(4));
    assert!(s.latest_known_with(&later, Duration::hours(3)).is_none()); // older than max_age
    assert_eq!(
        s.latest_known_with(&later, Duration::hours(6))
            .unwrap()
            .hour(),
        6
    );
}

#[test]
fn pt_hour_23_estimate_fills_the_last_hour_only() {
    let s = zone_series(Zone::Pt);
    let late = q_at(utc(2026, 11, 3, 23, 10)); // 23:10 Lisbon (UTC in November)
    assert!(s.now_with(&late).is_none());
    assert_eq!(
        s.now_with(&late.clone().with_estimate()).unwrap().price_mwh,
        Some(252.7)
    );
    let earlier = q_at(utc(2026, 11, 3, 22, 10)).with_estimate();
    assert_eq!(s.now_with(&earlier).unwrap().kind, PriceKind::Actual);
}

#[test]
fn estimate_never_replaces_a_settled_price() {
    let s = zone_series(Zone::Gb);
    let merged = s.all_slots(true);
    let plain = s.all_slots(false);
    assert_eq!(merged.len(), plain.len());
    for (m, p) in merged.iter().zip(&plain) {
        if p.price.is_some() {
            assert_eq!(m, p);
        }
    }
    assert_eq!(
        merged
            .iter()
            .filter(|sl| sl.kind == PriceKind::Estimate)
            .count(),
        17
    );
}

// -- days -------------------------------------------------------------------------------------

#[test]
fn today_and_tomorrow_in_the_zones_own_day() {
    let s = zone_series(Zone::De);
    let q = q_at(at());
    assert_eq!(hours(&s.today_with(&q)), (0..24).collect::<Vec<_>>());
    assert!(s
        .today_with(&q)
        .iter()
        .all(|sl| sl.local_date() == d(2026, 11, 3)));
    let tomorrow = s.tomorrow_with(&q);
    assert!(tomorrow.iter().all(|sl| sl.local_date() == d(2026, 11, 4)));
    // not published yet: a forecast, flagged
    assert!(tomorrow.iter().all(|sl| sl.kind == PriceKind::Forecast));
}

#[test]
fn today_uses_local_date_not_utc_date() {
    let s = zone_series(Zone::De);
    let q = q_at(utc(2026, 11, 3, 23, 30)); // already Nov 4 in Berlin
    assert_eq!(s.local_today_at(utc(2026, 11, 3, 23, 30)), d(2026, 11, 4));
    assert!(s
        .today_with(&q)
        .iter()
        .all(|sl| sl.local_date() == d(2026, 11, 4)));
}

#[test]
fn gb_has_no_published_tomorrow_but_forecast_days() {
    let s = zone_series(Zone::Gb);
    assert!(s
        .tomorrow_with(&q_at(at()))
        .iter()
        .all(|sl| sl.kind == PriceKind::Forecast));
    assert_eq!(s.day(d(2026, 11, 4)).unwrap().horizon, Some(1));
}

#[test]
fn xk_tomorrow_absent() {
    let s = zone_series(Zone::Xk);
    let q = q_at(utc(2026, 11, 3, 12, 0));
    assert!(s.tomorrow_with(&q).is_empty());
    assert_eq!(s.today_with(&q).len(), 24);
}

#[test]
fn stats() {
    let mut prices = some(&[10.0, 30.0, 20.0]);
    prices.push(None);
    prices.extend(some(&[25.0; 20]));
    let s = synthetic(&prices, JUN10, "actual", Zone::De, vec![]);
    let st = s.stats(d(2026, 6, 10)).unwrap();
    assert_eq!((st.minimum.hour(), st.maximum.hour(), st.count), (0, 1, 23));
    assert_close(
        st.average,
        (10.0 + 30.0 + 20.0 + 25.0 * 20.0) / 23.0 / 1000.0,
        1e-5,
    );
    assert!(s.stats(d(2000, 1, 1)).is_none());
}

// -- cheapest hours ---------------------------------------------------------------------------

fn day_query() -> Query {
    Query::new().day(d(2026, 6, 10))
}

#[test]
fn cheapest_hours_day_in_time_order() {
    let mut p = some(&[50.0, 40.0, 10.0, 30.0, 20.0, 60.0]);
    p.extend(some(&[100.0; 18]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    let got = s.cheapest_hours_with(3, &day_query()).unwrap();
    assert_eq!(hours(&got), [2, 3, 4]);
    let mwh: Vec<_> = got.iter().map(|sl| sl.price_mwh.unwrap()).collect();
    assert_eq!(mwh, [10.0, 30.0, 20.0]);
}

#[test]
fn cheapest_hours_tie_prefers_earliest() {
    let mut p = some(&[5.0; 4]);
    p.extend(some(&[9.0; 20]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    assert_eq!(
        hours(&s.cheapest_hours_with(2, &day_query()).unwrap()),
        [0, 1]
    );
}

#[test]
fn cheapest_hours_skips_unpriced_and_returns_fewer_when_short() {
    let mut p = vec![None; 22];
    p.extend(some(&[7.0, 3.0]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    assert_eq!(
        hours(&s.cheapest_hours_with(5, &day_query()).unwrap()),
        [22, 23]
    );
    assert!(s.cheapest_hours_with(0, &day_query()).unwrap().is_empty());
}

#[test]
fn cheapest_hours_default_window_is_the_next_24_hours() {
    let jun11 = day_json("2026-06-11", |h| 100.0 + h as f64);
    let s = synthetic(&some(&[1.0; 24]), JUN10, "actual", Zone::De, vec![jun11]); // Jun 10 super cheap, Jun 11 dearer
                                                                                  // 12:30 Berlin (CEST): window 12:00 .. 12:00 next day
    let got = s
        .cheapest_hours_with(2, &q_at(utc(2026, 6, 10, 10, 30)))
        .unwrap();
    let stamps: Vec<String> = got
        .iter()
        .map(|sl| sl.local_start().format("%d %H").to_string())
        .collect();
    assert_eq!(stamps, ["10 12", "10 13"]);
    let q = Query::new().between(utc(2026, 6, 10, 20, 0), utc(2026, 6, 11, 22, 0));
    let got = s.cheapest_hours_with(3, &q).unwrap();
    // 22:00 and 23:00 local on Jun 10, then 00:00 on Jun 11
    let mwh: Vec<_> = got.iter().map(|sl| sl.price_mwh.unwrap()).collect();
    assert_eq!(mwh, [1.0, 1.0, 100.0]);
}

#[test]
fn cheapest_hours_start_and_end_builders() {
    let jun11 = day_json("2026-06-11", |h| 100.0 + h as f64);
    let s = synthetic(&some(&[1.0; 24]), JUN10, "actual", Zone::De, vec![jun11]);
    let q = Query::new()
        .start(utc(2026, 6, 10, 20, 0))
        .end(utc(2026, 6, 10, 22, 0));
    assert_eq!(s.cheapest_hours_with(10, &q).unwrap().len(), 2);
}

#[test]
fn cheapest_hours_settled_only_excludes_forecast() {
    let s = zone_series(Zone::De);
    let with_fc = s.cheapest_hours_with(4, &q_at(at())).unwrap();
    let only = s
        .cheapest_hours_with(4, &q_at(at()).settled_only())
        .unwrap();
    assert!(only.iter().all(|sl| sl.kind == PriceKind::Actual));
    assert_eq!(only.len(), 4);
    assert!(with_fc.iter().any(|sl| sl.kind == PriceKind::Forecast) || with_fc != only);
}

#[test]
fn cheapest_hours_validation() {
    let s = synthetic(&some(&[1.0; 24]), JUN10, "actual", Zone::De, vec![]);
    let q = Query::new()
        .day(d(2026, 6, 10))
        .start(utc(2026, 6, 10, 0, 0));
    let err = s.cheapest_hours_with(1, &q).unwrap_err();
    assert!(matches!(&err, Error::InvalidArgument(m) if m.contains("either day or start/end")));
    assert!(matches!(
        s.best_window_with(1, &q),
        Err(Error::InvalidArgument(_))
    ));
    // the plain method never fails: it returns what it has
    assert!(s.cheapest_hours(3).len() <= 3);
}

#[test]
fn cheapest_hours_with_estimate_flag() {
    let s = zone_series(Zone::Gb);
    let day = Query::new().day(d(2026, 11, 3));
    let plain = s.cheapest_hours_with(3, &day).unwrap();
    assert!(plain.iter().all(|sl| sl.kind == PriceKind::Actual));
    assert_eq!(plain.len(), 3);
    let mixed = s.cheapest_hours_with(24, &day.with_estimate()).unwrap();
    assert_eq!(mixed.len(), 24);
    assert!(mixed.iter().any(|sl| sl.kind == PriceKind::Estimate));
}

// -- best window ------------------------------------------------------------------------------

#[test]
fn best_window_finds_cheapest_consecutive_run() {
    let mut p = some(&[10.0, 90.0, 20.0, 20.0, 20.0, 90.0, 15.0, 15.0, 95.0]);
    p.extend(some(&[100.0; 15]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    let w = s.best_window_with(3, &day_query()).unwrap().unwrap();
    assert_eq!(
        (
            w.local_start().format("%H").to_string(),
            w.local_end().format("%H").to_string(),
            w.hours()
        ),
        ("02".into(), "05".into(), 3)
    );
    assert_close(w.average, 0.02, 1e-12);
    let mwh: Vec<_> = w.slots.iter().map(|sl| sl.price_mwh.unwrap()).collect();
    assert_eq!(mwh, [20.0, 20.0, 20.0]);
}

#[test]
fn best_window_is_not_the_same_as_cheapest_hours() {
    let mut p = some(&[1.0, 99.0, 1.0, 99.0, 1.0, 99.0, 20.0, 20.0, 20.0]);
    p.extend(some(&[200.0; 15]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    assert_eq!(
        hours(&s.cheapest_hours_with(3, &day_query()).unwrap()),
        [0, 2, 4]
    );
    let w = s.best_window_with(3, &day_query()).unwrap().unwrap();
    assert_eq!(w.slots[0].hour(), 6);
}

#[test]
fn best_window_does_not_bridge_gaps() {
    let mut p = some(&[1.0, 1.0]);
    p.push(None);
    p.extend(some(&[1.0, 1.0]));
    p.extend(some(&[100.0; 19]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![]);
    // hour 2 is unpriced: 0-1 and 3-4 cannot be joined, 3-5 is the best run
    let w = s.best_window_with(3, &day_query()).unwrap().unwrap();
    assert_eq!(w.slots[0].hour(), 3);
    assert_close(w.average, 0.034, 1e-12);
    assert_eq!(
        s.best_window_with(2, &day_query()).unwrap().unwrap().slots[0].hour(),
        0
    );
}

#[test]
fn best_window_tie_earliest_and_none_when_impossible() {
    let s = synthetic(&some(&[4.0; 24]), JUN10, "actual", Zone::De, vec![]);
    assert_eq!(
        s.best_window_with(5, &day_query()).unwrap().unwrap().slots[0].hour(),
        0
    );
    assert!(s.best_window_with(25, &day_query()).unwrap().is_none());
    assert!(synthetic(&[None; 24], JUN10, "actual", Zone::De, vec![])
        .best_window_with(1, &day_query())
        .unwrap()
        .is_none());
    assert!(matches!(s.best_window(0), Err(Error::InvalidArgument(_))));
}

#[test]
fn best_window_across_midnight() {
    let next: Value = json!({
        "date": "2026-06-11",
        "type": "actual",
        "prices": (0..24).map(|h| json!({"hour": h, "eur_mwh": if h < 2 { 1 } else { 500 }})).collect::<Vec<_>>(),
    });
    let mut p = some(&[500.0; 22]);
    p.extend(some(&[1.0, 1.0]));
    let s = synthetic(&p, JUN10, "actual", Zone::De, vec![next]);
    let q = Query::new().between(utc(2026, 6, 10, 20, 0), utc(2026, 6, 12, 0, 0));
    let w = s.best_window_with(4, &q).unwrap().unwrap();
    assert_eq!(w.local_start().date_naive(), d(2026, 6, 10));
    assert_eq!(
        (
            w.local_start().format("%H").to_string(),
            w.local_end().format("%H").to_string()
        ),
        ("22".into(), "02".into())
    );
}

// -- DST --------------------------------------------------------------------------------------

#[test]
fn day_with_25_hours_doubles_hour_2() {
    let prices: Vec<Option<f64>> = (0..24).map(|h| Some(h as f64)).collect();
    let s = synthetic(&prices, "2026-10-25", "actual", Zone::Dk1, vec![]); // clocks go back in Denmark
    let slots = &s.day(d(2026, 10, 25)).unwrap().slots;
    assert_eq!(slots.len(), 25);
    assert_eq!(hours(&slots[..5]), [0, 1, 2, 2, 3]);
    assert_eq!(slots[2].price_mwh, slots[3].price_mwh);
    assert_eq!(slots[2].price_mwh, Some(2.0));
    // UTC instants: the doubled hour is a real hour
    assert_eq!(slots[3].start - slots[2].start, Duration::hours(1));
    let unique: std::collections::BTreeSet<_> = slots.iter().map(|sl| sl.start).collect();
    assert_eq!(unique.len(), 25);
}

#[test]
fn day_with_23_hours_has_no_hour_2() {
    let prices: Vec<Option<f64>> = (0..24).map(|h| Some(h as f64)).collect();
    let s = synthetic(&prices, "2026-03-29", "actual", Zone::Dk1, vec![]);
    let slots = &s.day(d(2026, 3, 29)).unwrap().slots;
    assert_eq!(slots.len(), 23);
    assert!(!hours(slots).contains(&2));
    assert_eq!(hours(&slots[..4]), [0, 1, 3, 4]);
    assert_eq!(slots[1].end, slots[2].start); // no gap on the timeline
}

#[test]
fn window_arithmetic_over_dst_uses_elapsed_time() {
    let s = synthetic(
        &some(&[10.0; 24]),
        "2026-10-25",
        "actual",
        Zone::Dk1,
        vec![],
    );
    let w = s
        .best_window_with(25, &Query::new().day(d(2026, 10, 25)))
        .unwrap()
        .unwrap();
    assert_eq!(w.end - w.start, Duration::hours(25));
}

// -- levels -----------------------------------------------------------------------------------

#[test]
fn price_levels_quintiles() {
    let prices: Vec<Option<f64>> = (1..=10).map(|p| Some(p as f64)).collect();
    // the API's cut points are the sorted values at index floor(n*f): 3, 5, 7, 9 for ten prices
    let mut expected = vec![PriceLevel::VeryCheap; 3];
    expected.extend([PriceLevel::Cheap; 2]);
    expected.extend([PriceLevel::Normal; 2]);
    expected.extend([PriceLevel::Expensive; 2]);
    expected.push(PriceLevel::VeryExpensive);
    assert_eq!(price_levels(&prices), expected);
}

#[test]
fn price_levels_missing_and_flat() {
    assert_eq!(
        price_levels(&[None, None]),
        [PriceLevel::Normal, PriceLevel::Normal]
    );
    // same as the API: p <= p20
    assert_eq!(price_levels(&some(&[3.0; 5])), [PriceLevel::VeryCheap; 5]);
    assert_eq!(
        price_levels(&[Some(1.0), None, Some(9.0)])[1],
        PriceLevel::Normal
    );
    assert!(price_levels(&[]).is_empty());
}

#[test]
fn series_levels_per_local_day() {
    let s = zone_series(Zone::De);
    let levels = s.levels_with(&q_at(at()));
    assert_eq!(levels.len(), 24);
    assert!(
        levels
            .iter()
            .filter(|(_, lv)| *lv == PriceLevel::VeryCheap)
            .count()
            >= 4
    );
    let by_price = |a: &&(elpriser::Slot, PriceLevel), b: &&(elpriser::Slot, PriceLevel)| {
        a.0.price.unwrap().total_cmp(&b.0.price.unwrap())
    };
    let cheapest = levels.iter().min_by(by_price).unwrap();
    assert_eq!(cheapest.1, PriceLevel::VeryCheap);
    assert_eq!(s.level(&cheapest.0), Some(PriceLevel::VeryCheap));
    let dearest = levels.iter().max_by(by_price).unwrap();
    assert_eq!(s.level(&dearest.0), Some(PriceLevel::VeryExpensive));
}

#[test]
fn level_of_unpriced_slot_is_none() {
    let s = zone_series(Zone::Gb);
    assert_eq!(s.level(&s.slot_at(&q_at(at())).unwrap()), None);
}

/// The Danish feed's day 1 is exactly /api/shelly/tariff's `today`; our levels must agree.
#[test]
fn dk_levels_equal_the_apis_own_tibber_levels() {
    let s = Series::from_dk_forecast_json(
        Zone::Dk1,
        &load("dk_forecast"),
        elpriser::Mode::InklAlt,
        None,
    )
    .unwrap();
    let api = elpriser::parse::parse_shelly_tariff(&load("dk_shelly"))
        .unwrap()
        .today;
    let mine = s.levels_with(&Query::new().day(d(2026, 11, 3)));
    let rounded: Vec<f64> = mine
        .iter()
        .map(|(sl, _)| (sl.price.unwrap() * 1e4).round() / 1e4)
        .collect();
    let totals: Vec<f64> = api.iter().map(|t| t.total.unwrap()).collect();
    assert_eq!(rounded, totals);
    let levels: Vec<_> = mine.iter().map(|(_, lv)| Some(*lv)).collect();
    let theirs: Vec<_> = api.iter().map(|t| t.level).collect();
    assert_eq!(levels, theirs);
}
