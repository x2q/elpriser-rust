//! The async client (feature `async`) against a real local HTTP server serving recorded answers.
#![cfg(feature = "async")]
mod common;

use std::time::Duration;

use chrono::NaiveDate;
use common::{at, load, Reply, TestServer};
use elpriser::{AsyncClient, Charges, DkQuery, Error, Mode, PriceKind, Query, Strategy, Zone};
use serde_json::json;

/// A server that answers every API path with its recorded fixture.
fn fixture_server() -> TestServer {
    TestServer::start(|_, req| {
        let (path, params) = req
            .target
            .split_once('?')
            .unwrap_or((req.target.as_str(), ""));
        let has = |needle: &str| params.contains(needle);
        let name = match path {
            "/api/nordic" => {
                let zone = params
                    .strip_prefix("zone=")
                    .unwrap_or("de")
                    .replace('-', "_");
                return Reply::json(200, &load(&format!("zone_{zone}")));
            }
            "/api/forecast" if has("mode=net_") => "dk_forecast_net",
            "/api/forecast" => "dk_forecast",
            "/api/now" => "dk_now",
            "/api/prices" => "dk_prices",
            "/api/schedule" => "dk_schedule",
            "/api/shelly/tariff" => "dk_shelly",
            "/api/supplierlookup" => "supplier_ok",
            _ => return Reply::json(404, &json!({"error": "no such fixture"})),
        };
        Reply::json(200, &load(name))
    })
}

fn client(server: &TestServer) -> AsyncClient {
    AsyncClient::builder()
        .base_url(&server.base_url)
        .backoff(Duration::ZERO)
        .build()
        .unwrap()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[tokio::test]
async fn series_and_questions() {
    let server = fixture_server();
    let c = client(&server);
    let q = Query::new().at(at());
    let s = c.series("de").await.unwrap();
    assert_eq!((s.zone, s.currency.as_str()), (Zone::De, "EUR"));
    assert_eq!(
        c.now_with(Zone::De, &q).await.unwrap().unwrap().price_mwh,
        Some(301.31)
    );
    assert_eq!(c.next_hour_with("de", &q).await.unwrap().unwrap().hour(), 9);
    assert_eq!(c.today_with("de", &q).await.unwrap().len(), 24);
    assert_eq!(
        c.tomorrow_with("de", &q).await.unwrap()[0].kind,
        PriceKind::Forecast
    );
    let day = Query::new().day(d(2026, 11, 3));
    assert_eq!(c.cheapest_hours_with("de", 2, &day).await.unwrap().len(), 2);
    assert_eq!(
        c.best_window_with("de", 3, &day)
            .await
            .unwrap()
            .unwrap()
            .hours(),
        3
    );
    assert_eq!(c.forecast("de").await.unwrap().len(), 6);
    assert_eq!(c.levels_with("de", &q).await.unwrap().len(), 24);
    assert!(c.zone_feed("dk1").await.unwrap().rate.is_some());
    // the plain methods use the real clock; only check they run
    c.now("de").await.unwrap();
    c.next_hour("de").await.unwrap();
    c.today("de").await.unwrap();
    c.tomorrow("de").await.unwrap();
    c.cheapest_hours("de", 3).await.unwrap();
    c.best_window("de", 3).await.unwrap();
    c.levels("de").await.unwrap();
    let gb = c.now_with("gb", &q.with_estimate()).await.unwrap().unwrap();
    assert_eq!(gb.kind, PriceKind::Estimate);
}

#[tokio::test]
async fn sends_the_user_agent_and_the_right_query() {
    let server = fixture_server();
    let c = client(&server);
    c.series_with(
        "dk2",
        &Query::new().mode(Mode::NetInklAlt).gln("5790000704842"),
    )
    .await
    .unwrap();
    let seen = server.requests();
    assert_eq!(
        seen[0].headers["user-agent"],
        format!("elpriser-rust/{}", elpriser::VERSION)
    );
    assert_eq!(
        seen[0].target,
        "/api/forecast?area=DK2&mode=net_inkl_alt&gln=5790000704842"
    );
    assert_eq!(
        c.user_agent(),
        format!("elpriser-rust/{}", elpriser::VERSION)
    );
    assert_eq!(c.base_url(), server.base_url);
}

#[tokio::test]
async fn dk_endpoints() {
    let server = fixture_server();
    let c = client(&server);
    let now = c
        .dk_now_with(
            "DK1",
            &DkQuery::new().strategy(Strategy::CheapestN).hours(6),
        )
        .await
        .unwrap();
    assert_eq!((now.price, now.hour), (Some(2.9896), 8));
    assert_eq!(
        server.requests()[0].target,
        "/api/now?area=DK1&mode=inkl_alt&strategy=cheapest_n&hours=6"
    );
    let prices = c
        .dk_prices_with(
            Zone::Dk2,
            &DkQuery::new()
                .date(d(2026, 11, 3))
                .charges(Charges::Current),
        )
        .await
        .unwrap();
    assert_eq!(prices.prices.len(), 24);
    let sched = c.dk_schedule("dk1").await.unwrap();
    assert_eq!(sched.on_hours(), [0, 1, 2, 3, 4, 5]);
    assert_eq!(c.shelly_tariff("DK1").await.unwrap().today.len(), 24);
    assert!(c.supplier_lookup(55.6761, 12.5683).await.unwrap().found());
    c.dk_now("dk1").await.unwrap();
    c.dk_prices("dk1").await.unwrap();
    c.shelly_tariff_with("dk1", &DkQuery::new().mode(Mode::SpotEx))
        .await
        .unwrap();
}

#[tokio::test]
async fn validation_happens_before_any_request() {
    let server = fixture_server();
    let c = client(&server);
    assert!(matches!(c.now("narnia").await, Err(Error::UnknownZone(_))));
    assert!(matches!(
        c.dk_now("DK3").await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        c.series_with("de", &Query::new().mode(Mode::SpotEx)).await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        c.supplier_lookup(95.0, 0.0).await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn errors_are_mapped_and_4xx_is_not_retried() {
    let server = TestServer::start(|_, req| match req.target.as_str() {
        t if t.contains("zone=bad") => {
            Reply::json(400, &json!({"error": "zone must be one of: x"}))
        }
        t if t.contains("zone=nodata") => Reply::json(404, &json!({"error": "no prices"})),
        _ => Reply {
            status: 403,
            headers: vec![],
            body: b"<html>Cloudflare says no</html>".to_vec(),
        },
    });
    let c = client(&server);
    let bad = c
        .get_json("/api/nordic", &[("zone", "bad")])
        .await
        .unwrap_err();
    assert!(
        matches!(&bad, Error::BadRequest(h) if h.message.contains("must be one of")),
        "{bad:?}"
    );
    let none = c
        .get_json("/api/nordic", &[("zone", "nodata")])
        .await
        .unwrap_err();
    assert!(matches!(none, Error::NoData(_)));
    let blocked = c.get_json("/api/x", &[]).await.unwrap_err();
    assert!(matches!(&blocked, Error::Http(h) if h.message.contains("Cloudflare says no")));
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn retries_then_succeeds_and_gives_up() {
    let server = TestServer::start(|n, _| match n {
        1 => Reply::json(503, &json!({"error": "down"})).with_header("Retry-After", "0"),
        2 => Reply::json(429, &json!({"error": "slow"})).with_header("Retry-After", "0"),
        _ => Reply::json(200, &load("zone_de")),
    });
    let c = AsyncClient::builder()
        .base_url(&server.base_url)
        .retries(2)
        .backoff(Duration::ZERO)
        .build()
        .unwrap();
    assert_eq!(c.series("de").await.unwrap().zone, Zone::De);
    assert_eq!(server.requests().len(), 3);

    let always = TestServer::start(|_, _| Reply::json(500, &json!({"error": "boom"})));
    let c = AsyncClient::builder()
        .base_url(&always.base_url)
        .retries(1)
        .backoff(Duration::ZERO)
        .build()
        .unwrap();
    let err = c.series("de").await.unwrap_err();
    assert!(matches!(err, Error::Server(_)));
    assert_eq!(always.requests().len(), 2);
}

#[tokio::test]
async fn connection_refused_and_timeout() {
    let c = AsyncClient::builder()
        .base_url("http://127.0.0.1:9")
        .retries(0)
        .build()
        .unwrap();
    assert!(c.series("de").await.unwrap_err().is_network());

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
    });
    let c = AsyncClient::builder()
        .base_url(format!("http://{addr}"))
        .retries(0)
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    assert!(matches!(
        c.series("de").await.unwrap_err(),
        Error::Timeout { .. }
    ));
    handle.join().unwrap();
}

#[tokio::test]
async fn invalid_json_and_builder_validation() {
    let server = TestServer::start(|_, _| Reply {
        status: 200,
        headers: vec![],
        body: b"<html>captive portal</html>".to_vec(),
    });
    let c = client(&server);
    assert!(matches!(c.series("de").await.unwrap_err(), Error::Parse(_)));
    assert!(AsyncClient::builder()
        .timeout(Duration::ZERO)
        .build()
        .is_err());
    assert!(AsyncClient::builder().base_url("ftp://x").build().is_err());
    assert!(format!("{:?}", AsyncClient::new()).contains("elpriser.org"));
}
