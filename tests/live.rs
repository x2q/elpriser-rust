//! Opt-in checks against the real https://elpriser.org API (read-only GETs).
//!
//!     cargo test --test live -- --ignored
//!
//! They assert what the API documents, not today's prices. They are what keeps the zone table,
//! the parsers and the recorded fixtures honest. `cargo test` never runs them.

use elpriser::{Client, Mode, Zone};

fn api() -> Client {
    Client::builder()
        .user_agent("elpriser-rust-tests/live")
        .retries(3)
        .build()
        .unwrap()
}

#[test]
#[ignore = "talks to the real API"]
fn zone_enum_equals_the_apis_zone_list() {
    let spec = api().get_json("/api/openapi.json", &[]).unwrap();
    let listed: Vec<String> = spec["paths"]["/api/nordic"]["get"]["parameters"][0]["schema"]
        ["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let ours: Vec<String> = Zone::ALL.iter().map(|z| z.key().to_string()).collect();
    assert_eq!(ours, listed);
}

#[test]
#[ignore = "talks to the real API"]
fn modes_and_strategies_equal_the_apis() {
    let spec = api().get_json("/api/openapi.json", &[]).unwrap();
    let params = spec["paths"]["/api/schedule"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .clone();
    let enum_of = |name: &str| -> Vec<String> {
        let p = params.iter().find(|p| p["name"] == name).unwrap();
        p["schema"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let modes: Vec<String> = Mode::ALL.iter().map(|m| m.as_str().to_string()).collect();
    assert_eq!(modes, enum_of("mode"));
    let strategies: Vec<String> = elpriser::Strategy::ALL
        .iter()
        .map(|m| m.as_str().to_string())
        .collect();
    assert_eq!(strategies, enum_of("strategy"));
}

#[test]
#[ignore = "talks to the real API"]
fn a_zone_feed_parses_and_has_today() {
    let s = api().zone_feed(Zone::De).unwrap();
    assert_eq!(s.currency, Zone::De.currency());
    assert!(s.day(s.local_today()).is_some());
    assert!(s.today().iter().any(|slot| slot.price.is_some()));
}
