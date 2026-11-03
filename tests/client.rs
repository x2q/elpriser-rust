//! The blocking client against a fake transport serving recorded answers, plus the default
//! transport against a real local HTTP server.
mod common;

use std::time::Duration;

use chrono::NaiveDate;
use common::{at, error, fixture_client, load, ok, path, query, FakeTransport, Reply, TestServer};
use elpriser::{
    Charges, Client, DkQuery, Error, Mode, PriceKind, Query, Response, Strategy, TransportError,
    Zone,
};
use serde_json::json;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn qs(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn user_agent_and_headers() {
    let (client, t) = fixture_client();
    client.now_with(Zone::De, &Query::new().at(at())).unwrap();
    let call = &t.calls()[0];
    let header = |name: &str| {
        call.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(
        header("User-Agent"),
        Some(concat!("elpriser-rust/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(header("Accept"), Some("application/json"));
    assert_eq!(call.timeout, Duration::from_secs(10));
    assert!(call.url.starts_with("https://elpriser.org/"));
    assert_eq!(
        client.user_agent(),
        format!("elpriser-rust/{}", elpriser::VERSION)
    );
}

#[test]
fn custom_user_agent_timeout_and_base_url() {
    let t = FakeTransport::fixtures();
    let client = Client::builder()
        .base_url("http://localhost:8788/")
        .user_agent("my-app/2.0")
        .timeout(Duration::from_secs(3))
        .transport(t.clone())
        .build()
        .unwrap();
    client.series("de").unwrap();
    let call = &t.calls()[0];
    assert_eq!(call.url, "http://localhost:8788/api/nordic?zone=de");
    assert!(call
        .headers
        .contains(&("User-Agent".into(), "my-app/2.0".into())));
    assert_eq!(call.timeout, Duration::from_secs(3));
    assert_eq!(client.base_url(), "http://localhost:8788");
}

#[test]
fn builder_validation() {
    let bad_timeout = Client::builder().timeout(Duration::ZERO).build();
    assert!(matches!(bad_timeout, Err(Error::InvalidArgument(_))));
    for url in ["ftp://x", "file:///etc", "elpriser.org", "http://"] {
        let result = Client::builder().base_url(url).build();
        assert!(matches!(result, Err(Error::InvalidArgument(_))), "{url}");
    }
    assert!(Client::builder()
        .retries(0)
        .backoff(Duration::ZERO)
        .build()
        .is_ok());
    assert!(format!("{:?}", Client::new()).contains("https://elpriser.org"));
}

#[test]
fn non_danish_zone_uses_the_zone_feed() {
    let (client, t) = fixture_client();
    let s = client.series(Zone::Se3).unwrap();
    assert_eq!(path(&t.urls()[0]), "/api/nordic");
    assert_eq!(query(&t.urls()[0]), qs(&[("zone", "se3")]));
    assert_eq!(s.currency, "SEK");
}

#[test]
fn danish_zone_defaults_to_tariff_aware_forecast_feed() {
    let (client, t) = fixture_client();
    let s = client.series("dk1").unwrap();
    assert_eq!(path(&t.urls()[0]), "/api/forecast");
    assert_eq!(
        query(&t.urls()[0]),
        qs(&[("area", "DK1"), ("mode", "inkl_alt")])
    );
    assert_eq!(
        (s.source, s.currency.as_str()),
        (elpriser::SeriesSource::DkForecast, "DKK")
    );
}

#[test]
fn danish_mode_and_gln() {
    let (client, t) = fixture_client();
    let q = Query::new().mode(Mode::NetInklAlt).gln("5790000704842");
    let s = client.series_with(Zone::Dk2, &q).unwrap();
    assert_eq!(
        query(&t.urls()[0]),
        qs(&[
            ("area", "DK2"),
            ("mode", "net_inkl_alt"),
            ("gln", "5790000704842")
        ])
    );
    assert_eq!(
        (s.mode.as_deref(), s.gln.as_deref()),
        (Some("net_inkl_alt"), Some("5790000704842"))
    );
    assert!(s.days[0].grid_tariff.is_some());
    client
        .series_with("dk1", &Query::new().mode(Mode::SpotEx))
        .unwrap();
    assert_eq!(query(&t.urls()[1])["mode"], "spot_ex");
}

#[test]
fn zone_feed_for_a_danish_zone() {
    let (client, t) = fixture_client();
    let s = client.zone_feed("dk1").unwrap();
    assert_eq!(query(&t.urls()[0]), qs(&[("zone", "dk1")]));
    assert_eq!(
        (s.unit.as_str(), s.currency.as_str(), s.rate),
        ("EUR/MWh", "DKK", Some(7.4737))
    );
    assert_eq!(s.source, elpriser::SeriesSource::ZoneFeed);
}

#[test]
fn invalid_parameters_fail_before_any_request() {
    let (client, t) = fixture_client();
    let cases: Vec<(&str, Query, &str)> = vec![
        (
            "de",
            Query::new().mode(Mode::SpotEx),
            "only apply to the Danish",
        ),
        (
            "de",
            Query::new().gln("5790000704842"),
            "only apply to the Danish",
        ),
        ("dk1", Query::new().mode(Mode::NetInklAlt), "needs gln"),
    ];
    for (zone, q, expect) in cases {
        let err = client.series_with(zone, &q).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument(m) if m.contains(expect)),
            "{err}"
        );
    }
    assert!(t.calls().is_empty());
}

#[test]
fn unknown_zone_fails_before_any_request() {
    let (client, t) = fixture_client();
    assert!(matches!(client.now("narnia"), Err(Error::UnknownZone(_))));
    assert!(t.calls().is_empty());
}

#[test]
fn convenience_methods() {
    let (client, _) = fixture_client();
    let q = Query::new().at(at());
    assert_eq!(
        client.now_with("de", &q).unwrap().unwrap().price_mwh,
        Some(301.31)
    );
    assert_eq!(client.next_hour_with("de", &q).unwrap().unwrap().hour(), 9);
    assert_eq!(client.today_with("de", &q).unwrap().len(), 24);
    assert_eq!(
        client.tomorrow_with("de", &q).unwrap()[0].kind,
        PriceKind::Forecast
    );
    let day = Query::new().day(d(2026, 11, 3));
    let today = client.today_with("de", &q).unwrap();
    let mut expected = today.clone();
    expected.sort_by(|a, b| {
        a.price
            .unwrap()
            .total_cmp(&b.price.unwrap())
            .then(a.start.cmp(&b.start))
    });
    expected.truncate(2);
    expected.sort_by_key(|s| s.start);
    assert_eq!(client.cheapest_hours_with("de", 2, &day).unwrap(), expected);
    assert_eq!(
        client
            .best_window_with("de", 3, &day)
            .unwrap()
            .unwrap()
            .hours(),
        3
    );
    let horizons: Vec<_> = client
        .forecast("de")
        .unwrap()
        .iter()
        .map(|day| day.horizon)
        .collect();
    assert_eq!(horizons, [2, 3, 4, 5, 6, 7].map(Some));
    assert!(client
        .forecast("de")
        .unwrap()
        .iter()
        .all(|day| day.is_forecast()));
    assert_eq!(client.levels_with("de", &q).unwrap().len(), 24);
    // the plain methods use the real clock; the recorded data is from another day, so only check they run
    client.now("de").unwrap();
    client.next_hour("de").unwrap();
    client.today("de").unwrap();
    client.tomorrow("de").unwrap();
    client.cheapest_hours("de", 3).unwrap();
    client.best_window("de", 3).unwrap();
    client.levels("de").unwrap();
    client.series(Zone::De).unwrap();
}

#[test]
fn gb_now_needs_estimate_flag() {
    let (client, _) = fixture_client();
    let q = Query::new().at(at());
    assert!(client.now_with("gb", &q).unwrap().is_none());
    let est = client.now_with("gb", &q.with_estimate()).unwrap().unwrap();
    assert_eq!(est.kind, PriceKind::Estimate);
}

#[test]
fn dk_endpoints() {
    let (client, t) = fixture_client();
    let now = client
        .dk_now_with(
            "DK1",
            &DkQuery::new().strategy(Strategy::CheapestN).hours(6),
        )
        .unwrap();
    assert_eq!((now.price, now.hour), (Some(2.9896), 8));
    assert_eq!(
        query(t.urls().last().unwrap()),
        qs(&[
            ("area", "DK1"),
            ("mode", "inkl_alt"),
            ("strategy", "cheapest_n"),
            ("hours", "6")
        ])
    );
    let prices = client
        .dk_prices_with(
            Zone::Dk2,
            &DkQuery::new()
                .date(d(2026, 11, 3))
                .charges(Charges::Current),
        )
        .unwrap();
    assert_eq!(prices.prices.len(), 24);
    assert_eq!(
        query(t.urls().last().unwrap()),
        qs(&[
            ("area", "DK2"),
            ("mode", "inkl_alt"),
            ("date", "2026-11-03"),
            ("charges", "current")
        ])
    );
    let q = DkQuery::new()
        .strategy(Strategy::CheapestN)
        .hours(6)
        .date(d(2026, 11, 3));
    let sched = client.dk_schedule_with("dk1", &q).unwrap();
    assert_eq!(sched.on_hours(), [0, 1, 2, 3, 4, 5]);
    let last = t.urls().last().unwrap().clone();
    assert_eq!(query(&last)["date"], "2026-11-03");
    assert_eq!(path(&last), "/api/schedule");
    let tariff = client.shelly_tariff("DK1").unwrap();
    assert_eq!(tariff.today.len(), 24);
    assert_eq!(path(t.urls().last().unwrap()), "/api/shelly/tariff");
    let sup = client.supplier_lookup(55.6761, 12.5683).unwrap();
    assert!(sup.found());
    assert_eq!(
        query(t.urls().last().unwrap()),
        qs(&[("lat", "55.6761"), ("lng", "12.5683")])
    );
    client.dk_now(Zone::Dk1).unwrap();
    client.dk_prices("dk1").unwrap();
    client.dk_schedule(String::from("dk2")).unwrap();
    client
        .shelly_tariff_with("dk1", &DkQuery::new().mode(Mode::SpotEx))
        .unwrap();
}

#[test]
fn dk_parameter_validation() {
    let (client, t) = fixture_client();
    let msg = |e: Error| match e {
        Error::InvalidArgument(m) => m,
        other => panic!("expected InvalidArgument, got {other:?}"),
    };
    assert!(msg(client.dk_now("DK3").unwrap_err()).contains("area must be DK1 or DK2"));
    assert!(msg(client.dk_now("de").unwrap_err()).contains("area must be DK1 or DK2"));
    assert!(msg(client
        .dk_now_with("dk1", &DkQuery::new().hours(24))
        .unwrap_err())
    .contains("hours must be"));
    assert!(msg(client
        .dk_now_with("dk1", &DkQuery::new().hours(0))
        .unwrap_err())
    .contains("hours must be"));
    assert!(msg(client
        .dk_now_with("dk1", &DkQuery::new().pct(0))
        .unwrap_err())
    .contains("pct must be"));
    assert!(msg(client
        .dk_now_with("dk1", &DkQuery::new().pct(101))
        .unwrap_err())
    .contains("pct must be"));
    assert!(msg(client
        .dk_schedule_with("dk1", &DkQuery::new().max_off(13))
        .unwrap_err())
    .contains("max_off must be"));
    assert!(msg(client
        .dk_prices_with("dk1", &DkQuery::new().mode(Mode::NetInklTarif))
        .unwrap_err())
    .contains("needs gln"));
    assert!(msg(client.supplier_lookup(95.0, 0.0).unwrap_err()).contains("lat must be"));
    assert!(msg(client.supplier_lookup(0.0, f64::NAN).unwrap_err()).contains("lng must be"));
    // a field the endpoint does not take
    assert!(msg(client
        .dk_prices_with("dk1", &DkQuery::new().hours(3))
        .unwrap_err())
    .contains("does not take"));
    assert!(msg(client
        .dk_now_with("dk1", &DkQuery::new().charges(Charges::Current))
        .unwrap_err())
    .contains("does not take"));
    assert!(msg(client
        .shelly_tariff_with("dk1", &DkQuery::new().date(d(2026, 1, 1)))
        .unwrap_err())
    .contains("does not take"));
    assert!(t.calls().is_empty());
}

#[test]
fn get_json_escape_hatch() {
    let (client, t) = fixture_client();
    let value = client.get_json("/api/now", &[("area", "DK1")]).unwrap();
    assert_eq!(value["hour"], json!(8));
    assert_eq!(query(&t.urls()[0]), qs(&[("area", "DK1")]));
}

// -- errors -----------------------------------------------------------------------------------

#[test]
fn http_errors_are_not_retried() {
    for status in [400u16, 404, 403, 418] {
        let t = FakeTransport::queued(vec![Ok(error(status, "nope"))]);
        let client = Client::builder()
            .transport(t.clone())
            .backoff(Duration::ZERO)
            .build()
            .unwrap();
        let err = client.series("de").unwrap_err();
        match (status, &err) {
            (400, Error::BadRequest(_)) | (404, Error::NoData(_)) | (403 | 418, Error::Http(_)) => {
            }
            _ => panic!("{status}: {err:?}"),
        }
        let http = err.http().unwrap();
        assert_eq!((http.status, http.message.as_str()), (status, "nope"));
        assert!(http.url.contains("nordic"));
        assert_eq!(t.calls().len(), 1);
    }
}

#[test]
fn api_error_message_is_surfaced() {
    let t = FakeTransport::queued(vec![Ok(error(400, "zone must be one of: dk1, dk2"))]);
    let err = Client::builder()
        .transport(t)
        .build()
        .unwrap()
        .series("de")
        .unwrap_err();
    assert!(err.to_string().contains("zone must be one of"));
}

#[test]
fn non_json_error_body_is_summarised() {
    let t = FakeTransport::queued(vec![Ok(Response::new(
        403,
        "<html>Cloudflare says no</html>",
    ))]);
    let err = Client::builder()
        .transport(t)
        .build()
        .unwrap()
        .series("de")
        .unwrap_err();
    assert!(matches!(err, Error::Http(_)));
    assert!(err.to_string().contains("Cloudflare says no"));
}

#[test]
fn gives_up_after_retries_with_server_error() {
    let t = FakeTransport::queued(vec![Ok(error(503, "down").with_header("Retry-After", "0"))]);
    let client = Client::builder()
        .transport(t.clone())
        .retries(2)
        .build()
        .unwrap();
    let err = client.series("de").unwrap_err();
    assert!(matches!(err, Error::Server(_)));
    assert_eq!(
        (err.status(), err.retry_after()),
        (Some(503), Some(Duration::ZERO))
    );
    assert_eq!(t.calls().len(), 3);
}

#[test]
fn retries_zero_means_one_attempt() {
    let t = FakeTransport::queued(vec![Ok(error(500, "boom"))]);
    let client = Client::builder()
        .transport(t.clone())
        .retries(0)
        .build()
        .unwrap();
    assert!(matches!(client.series("de").unwrap_err(), Error::Server(_)));
    assert_eq!(t.calls().len(), 1);
}

#[test]
fn retries_5xx_then_succeeds() {
    let t = FakeTransport::queued(vec![
        Ok(error(503, "x")),
        Ok(error(502, "x")),
        Ok(ok(&load("zone_de"))),
    ]);
    let client = Client::builder()
        .transport(t.clone())
        .backoff(Duration::ZERO)
        .build()
        .unwrap();
    assert_eq!(client.series("de").unwrap().zone, Zone::De);
    assert_eq!(t.calls().len(), 3);
}

#[test]
fn rate_limit_error_after_retries() {
    let t = FakeTransport::queued(vec![Ok(
        error(429, "slow down").with_header("Retry-After", "0")
    )]);
    let client = Client::builder()
        .transport(t.clone())
        .retries(1)
        .build()
        .unwrap();
    let err = client.series("de").unwrap_err();
    assert!(matches!(err, Error::RateLimited(_)));
    assert_eq!(t.calls().len(), 2);
}

#[test]
fn timeout_is_retried_then_raised() {
    let t = FakeTransport::queued(vec![Err(TransportError::Timeout("read timed out".into()))]);
    let client = Client::builder()
        .transport(t.clone())
        .retries(2)
        .backoff(Duration::ZERO)
        .timeout(Duration::from_millis(1500))
        .build()
        .unwrap();
    let err = client.series("de").unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }));
    assert!(err.to_string().contains("1.5s"), "{err}");
    assert!(err.is_network());
    assert_eq!(t.calls().len(), 3);
}

#[test]
fn connection_error_is_retried_then_raised() {
    let reset = || Err(TransportError::Connection("reset".into()));
    let t = FakeTransport::queued(vec![reset(), reset(), Ok(ok(&load("zone_de")))]);
    let client = Client::builder()
        .transport(t)
        .backoff(Duration::ZERO)
        .retries(2)
        .build()
        .unwrap();
    assert_eq!(client.series("de").unwrap().zone, Zone::De);
    let t = FakeTransport::queued(vec![Err(TransportError::Connection(
        "name resolution failed".into(),
    ))]);
    let client = Client::builder()
        .transport(t)
        .backoff(Duration::ZERO)
        .retries(1)
        .build()
        .unwrap();
    let err = client.series("de").unwrap_err();
    assert!(matches!(&err, Error::Connection { .. }));
    assert!(err.to_string().contains("could not reach"));
}

#[test]
fn invalid_json_is_a_parse_error() {
    let t = FakeTransport::queued(vec![Ok(Response::new(200, "<html>captive portal</html>"))]);
    let err = Client::builder()
        .transport(t)
        .build()
        .unwrap()
        .series("de")
        .unwrap_err();
    assert!(
        matches!(&err, Error::Parse(m) if m.contains("did not return JSON")),
        "{err}"
    );
}

#[test]
fn wrong_shape_is_a_parse_error() {
    let t = FakeTransport::queued(vec![Ok(ok(&json!({"zone": "de", "unexpected": true})))]);
    let err = Client::builder()
        .transport(t)
        .build()
        .unwrap()
        .series("de")
        .unwrap_err();
    assert!(matches!(err, Error::Parse(_)));
}

#[test]
fn closures_are_transports() {
    let client = Client::builder()
        .transport(|_: &str, _: &[(&str, &str)], _: Duration| Ok(ok(&load("zone_xk"))))
        .build()
        .unwrap();
    assert_eq!(client.series("xk").unwrap().zone, Zone::Xk);
}

#[test]
fn client_is_clone_send_and_sync() {
    fn check<T: Send + Sync>() {}
    fn check_clone<T: Clone>() {}
    check::<Client>();
    check_clone::<Client>();
    check::<Error>();
    check::<elpriser::Series>();
}

// -- the default transport against a real local HTTP server --------------------------------------

#[test]
fn ureq_transport_end_to_end() {
    let server = TestServer::start(|_, req| {
        if req.target.starts_with("/api/nordic?zone=de") {
            Reply::json(200, &load("zone_de"))
        } else {
            Reply::json(400, &json!({"error": "zone must be one of: x"}))
        }
    });
    let client = Client::builder()
        .base_url(&server.base_url)
        .retries(0)
        .build()
        .unwrap();
    assert_eq!(client.series("de").unwrap().zone, Zone::De);
    let seen = server.requests();
    assert_eq!(
        seen[0].headers["user-agent"],
        format!("elpriser-rust/{}", elpriser::VERSION)
    );
    assert_eq!(seen[0].headers["accept"], "application/json");
    assert_eq!(seen[0].target, "/api/nordic?zone=de");
    let err = client
        .get_json("/api/nordic", &[("zone", "zz")])
        .unwrap_err();
    assert!(matches!(err, Error::BadRequest(_)));
}

#[test]
fn ureq_transport_retries_and_honours_retry_after() {
    let server = TestServer::start(|n, _| match n {
        1 => Reply::json(503, &json!({"error": "down"})).with_header("Retry-After", "0"),
        2 => Reply::json(429, &json!({"error": "slow"})).with_header("Retry-After", "0"),
        _ => Reply::json(200, &load("zone_de")),
    });
    let client = Client::builder()
        .base_url(&server.base_url)
        .retries(2)
        .backoff(Duration::ZERO)
        .build()
        .unwrap();
    assert_eq!(client.series("de").unwrap().zone, Zone::De);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn ureq_transport_connection_refused() {
    let client = Client::builder()
        .base_url("http://127.0.0.1:9")
        .retries(0)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    assert!(matches!(
        client.series("de").unwrap_err(),
        Error::Connection { .. } | Error::Timeout { .. }
    ));
}

#[test]
fn ureq_transport_times_out() {
    // A server that accepts but never answers.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
    });
    let client = Client::builder()
        .base_url(format!("http://{addr}"))
        .retries(0)
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let err = client.series("de").unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
    handle.join().unwrap();
}
