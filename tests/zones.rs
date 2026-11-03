mod common;

use elpriser::{Error, IntoZone, Zone, ZoneGroup, ZONES};

/// The zone enum of https://elpriser.org/api/openapi.json (checked live in tests/live.rs).
const API_ZONES: [&str; 40] = [
    "dk1", "dk2", "no1", "no2", "no3", "no4", "no5", "se1", "se2", "se3", "se4", "fi", "nl", "de",
    "fr", "pl", "it-n", "at", "be", "ch", "cz", "hu", "si", "es", "pt", "sk", "gr", "ro", "bg",
    "hr", "rs", "ee", "lv", "lt", "me", "mk", "al", "xk", "gb", "ie",
];

#[test]
fn all_forty_zones_in_api_order() {
    let keys: Vec<&str> = ZONES.iter().map(|z| z.key()).collect();
    assert_eq!(keys, API_ZONES);
    assert_eq!(Zone::ALL.len(), 40);
    assert_eq!(ZONES, Zone::ALL);
}

#[test]
fn every_zone_has_valid_metadata() {
    for zone in Zone::ALL {
        assert_eq!(zone.currency().len(), 3, "{zone}");
        assert_eq!(zone.country().len(), 2, "{zone}");
        assert!(!zone.label().is_empty());
        assert!(!zone.tz().name().is_empty());
        assert_eq!(zone.area().is_some(), zone.group() == ZoneGroup::Dk);
        assert_eq!(zone.to_string(), zone.key());
    }
}

#[test]
fn zone_clocks() {
    // The Nordic/NL zones are on Copenhagen time, except Finland: Helsinki's days (ENTSO-E 2026-11-03).
    for z in Zone::ALL
        .iter()
        .filter(|z| z.group() != ZoneGroup::Eu && **z != Zone::Fi)
    {
        assert_eq!(z.tz().name(), "Europe/Copenhagen", "{z}");
    }
    assert_eq!(Zone::Fi.tz().name(), "Europe/Helsinki");
    assert_eq!(Zone::De.tz().name(), "Europe/Berlin");
    assert_eq!(Zone::Gb.tz().name(), "Europe/London");
    assert_eq!(Zone::Pt.tz().name(), "Europe/Lisbon");
    assert_eq!(Zone::Xk.tz().name(), "Europe/Belgrade");
    assert_eq!(Zone::Se3.label(), "SE3 Stockholm");
    assert_eq!(Zone::Dk2.label(), "DK2 Østdanmark");
}

#[test]
fn currencies_other_than_euro() {
    let non_eur: Vec<(&str, &str)> = Zone::ALL
        .iter()
        .filter(|z| z.currency() != "EUR")
        .map(|z| (z.key(), z.currency()))
        .collect();
    assert_eq!(
        non_eur,
        [
            ("dk1", "DKK"),
            ("dk2", "DKK"),
            ("no1", "NOK"),
            ("no2", "NOK"),
            ("no3", "NOK"),
            ("no4", "NOK"),
            ("no5", "NOK"),
            ("se1", "SEK"),
            ("se2", "SEK"),
            ("se3", "SEK"),
            ("se4", "SEK"),
            ("pl", "PLN"),
            ("gb", "GBP"),
        ]
    );
}

#[test]
fn danish_zones() {
    assert_eq!(Zone::Dk1.area(), Some("DK1"));
    assert_eq!(Zone::Dk2.area(), Some("DK2"));
    let danish: Vec<Zone> = Zone::ALL.into_iter().filter(|z| z.is_danish()).collect();
    assert_eq!(danish, [Zone::Dk1, Zone::Dk2]);
    assert_eq!(danish, Zone::DANISH);
}

#[test]
fn parse_accepts_the_usual_spellings() {
    for (text, expected) in [
        ("de", Zone::De),
        ("DE", Zone::De),
        (" de ", Zone::De),
        ("it-n", Zone::ItN),
        ("IT_N", Zone::ItN),
        ("it_n", Zone::ItN),
    ] {
        assert_eq!(Zone::parse(text).unwrap(), expected, "{text}");
        assert_eq!(text.parse::<Zone>().unwrap(), expected);
    }
}

#[test]
fn parse_rejects_unknown() {
    for bad in ["xx", "", "dk3", "it", "d e"] {
        let err = Zone::parse(bad).unwrap_err();
        assert!(matches!(err, Error::UnknownZone(_)), "{bad}");
        assert!(err.to_string().contains("valid zones"));
        assert!(err.to_string().contains("it-n"));
    }
}

#[test]
fn every_key_round_trips() {
    for zone in Zone::ALL {
        assert_eq!(zone.key().parse::<Zone>().unwrap(), zone);
        assert_eq!(zone.key().to_uppercase().parse::<Zone>().unwrap(), zone);
    }
}

#[test]
fn into_zone_takes_zones_and_strings() {
    assert_eq!(Zone::Se3.into_zone().unwrap(), Zone::Se3);
    assert_eq!((&Zone::Se3).into_zone().unwrap(), Zone::Se3);
    assert_eq!("se3".into_zone().unwrap(), Zone::Se3);
    assert_eq!(String::from("SE3").into_zone().unwrap(), Zone::Se3);
    assert_eq!((&String::from("se3")).into_zone().unwrap(), Zone::Se3);
    assert!("narnia".into_zone().is_err());
}
