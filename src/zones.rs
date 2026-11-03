//! The price zones elpriser.org serves.
//!
//! The list mirrors the `zone` enum of <https://elpriser.org/api/openapi.json> (40 zones).
//! [`Zone::tz`] is the wall clock the API's `hour` field (0-23) is expressed in: Copenhagen's
//! for DK, NO, SE and NL, Helsinki's for Finland (elpriser.org cuts `fi` into Helsinki days;
//! until then the feed floored it to the Copenhagen hour, which 0.1.0 of this crate assumed),
//! every other zone is on its own country's clock.

use std::fmt;
use std::str::FromStr;

use chrono_tz::Tz;

use crate::error::{Error, Result};

/// Which feed family a zone belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ZoneGroup {
    /// DK1 and DK2: also have the tariff-aware endpoints.
    Dk,
    /// Norway, Sweden, Finland and the Netherlands.
    Nordic,
    /// Every other zone.
    Eu,
}

impl ZoneGroup {
    /// `"dk"`, `"nordic"` or `"eu"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ZoneGroup::Dk => "dk",
            ZoneGroup::Nordic => "nordic",
            ZoneGroup::Eu => "eu",
        }
    }
}

const ZONE_COUNT: usize = 40;

macro_rules! zones {
    ($($variant:ident => $key:literal, $label:literal, $country:literal, $currency:literal,
       $tz:ident, $group:ident, $area:expr;)+) => {
        /// A bidding zone. Its [`key`](Zone::key) is what the API uses (`Zone::ItN.key() == "it-n"`).
        ///
        /// Every client method takes a `Zone` or a string (`"de"`, `"DE"`, `"it-n"`, `"IT_N"`),
        /// see [`IntoZone`].
        ///
        /// ```
        /// use elpriser::Zone;
        ///
        /// let zone: Zone = "se3".parse()?;
        /// assert_eq!(zone, Zone::Se3);
        /// assert_eq!(zone.to_string(), "se3");
        /// assert_eq!(zone.currency(), "SEK");
        /// assert_eq!(zone.tz().name(), "Europe/Copenhagen");
        /// assert_eq!(Zone::ALL.len(), 40);
        /// # Ok::<(), elpriser::Error>(())
        /// ```
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[non_exhaustive]
        pub enum Zone {
            $(
                #[doc = $label]
                $variant,
            )+
        }

        impl Zone {
            /// All zones, in the API's order.
            pub const ALL: [Zone; ZONE_COUNT] = [$(Zone::$variant),+];

            /// The key the API uses: `"dk1"`, `"de"`, `"it-n"` ...
            pub const fn key(self) -> &'static str {
                match self { $(Zone::$variant => $key),+ }
            }

            /// Human-readable name as the API reports it (`zoneInfo.name`).
            pub const fn label(self) -> &'static str {
                match self { $(Zone::$variant => $label),+ }
            }

            /// Country code (`"XK"` for Kosovo).
            pub const fn country(self) -> &'static str {
                match self { $(Zone::$variant => $country),+ }
            }

            /// The local currency the API converts to (`zoneInfo.currency`).
            pub const fn currency(self) -> &'static str {
                match self { $(Zone::$variant => $currency),+ }
            }

            /// The wall clock the API's `hour` field is expressed in. This is also what
            /// "today" and "tomorrow" mean for the zone.
            pub const fn tz(self) -> Tz {
                match self { $(Zone::$variant => Tz::$tz),+ }
            }

            /// Which feed family the zone belongs to.
            pub const fn group(self) -> ZoneGroup {
                match self { $(Zone::$variant => ZoneGroup::$group),+ }
            }

            /// `Some("DK1")` / `Some("DK2")` for the two Danish zones (they also have the
            /// tariff-aware endpoints), `None` for every other zone.
            pub const fn area(self) -> Option<&'static str> {
                match self { $(Zone::$variant => $area),+ }
            }
        }
    };
}

zones! {
    Dk1 => "dk1", "DK1 Vestdanmark", "DK", "DKK", Europe__Copenhagen, Dk, Some("DK1");
    Dk2 => "dk2", "DK2 Østdanmark", "DK", "DKK", Europe__Copenhagen, Dk, Some("DK2");
    No1 => "no1", "NO1 Oslo", "NO", "NOK", Europe__Copenhagen, Nordic, None;
    No2 => "no2", "NO2 Kristiansand", "NO", "NOK", Europe__Copenhagen, Nordic, None;
    No3 => "no3", "NO3 Trondheim", "NO", "NOK", Europe__Copenhagen, Nordic, None;
    No4 => "no4", "NO4 Tromsø", "NO", "NOK", Europe__Copenhagen, Nordic, None;
    No5 => "no5", "NO5 Bergen", "NO", "NOK", Europe__Copenhagen, Nordic, None;
    Se1 => "se1", "SE1 Luleå", "SE", "SEK", Europe__Copenhagen, Nordic, None;
    Se2 => "se2", "SE2 Sundsvall", "SE", "SEK", Europe__Copenhagen, Nordic, None;
    Se3 => "se3", "SE3 Stockholm", "SE", "SEK", Europe__Copenhagen, Nordic, None;
    Se4 => "se4", "SE4 Malmö", "SE", "SEK", Europe__Copenhagen, Nordic, None;
    Fi => "fi", "FI Finland", "FI", "EUR", Europe__Helsinki, Nordic, None;
    Nl => "nl", "NL Nederland", "NL", "EUR", Europe__Copenhagen, Nordic, None;
    De => "de", "DE-LU Germany and Luxembourg", "DE", "EUR", Europe__Berlin, Eu, None;
    Fr => "fr", "FR France", "FR", "EUR", Europe__Paris, Eu, None;
    Pl => "pl", "PL Poland", "PL", "PLN", Europe__Warsaw, Eu, None;
    ItN => "it-n", "IT-North", "IT", "EUR", Europe__Rome, Eu, None;
    At => "at", "AT Austria", "AT", "EUR", Europe__Vienna, Eu, None;
    Be => "be", "BE Belgium", "BE", "EUR", Europe__Brussels, Eu, None;
    Ch => "ch", "CH Switzerland", "CH", "EUR", Europe__Zurich, Eu, None;
    Cz => "cz", "CZ Czechia", "CZ", "EUR", Europe__Prague, Eu, None;
    Hu => "hu", "HU Hungary", "HU", "EUR", Europe__Budapest, Eu, None;
    Si => "si", "SI Slovenia", "SI", "EUR", Europe__Ljubljana, Eu, None;
    Es => "es", "ES Spain", "ES", "EUR", Europe__Madrid, Eu, None;
    Pt => "pt", "PT Portugal", "PT", "EUR", Europe__Lisbon, Eu, None;
    Sk => "sk", "SK Slovakia", "SK", "EUR", Europe__Bratislava, Eu, None;
    Gr => "gr", "GR Greece", "GR", "EUR", Europe__Athens, Eu, None;
    Ro => "ro", "RO Romania", "RO", "EUR", Europe__Bucharest, Eu, None;
    Bg => "bg", "BG Bulgaria", "BG", "EUR", Europe__Sofia, Eu, None;
    Hr => "hr", "HR Croatia", "HR", "EUR", Europe__Zagreb, Eu, None;
    Rs => "rs", "RS Serbia", "RS", "EUR", Europe__Belgrade, Eu, None;
    Ee => "ee", "EE Estonia", "EE", "EUR", Europe__Tallinn, Eu, None;
    Lv => "lv", "LV Latvia", "LV", "EUR", Europe__Riga, Eu, None;
    Lt => "lt", "LT Lithuania", "LT", "EUR", Europe__Vilnius, Eu, None;
    Me => "me", "ME Montenegro", "ME", "EUR", Europe__Podgorica, Eu, None;
    Mk => "mk", "MK North Macedonia", "MK", "EUR", Europe__Skopje, Eu, None;
    Al => "al", "AL Albania", "AL", "EUR", Europe__Tirane, Eu, None;
    Xk => "xk", "XK Kosovo", "XK", "EUR", Europe__Belgrade, Eu, None;
    Gb => "gb", "GB Great Britain", "GB", "GBP", Europe__London, Eu, None;
    Ie => "ie", "IE Ireland (SEM)", "IE", "EUR", Europe__Dublin, Eu, None;
}

/// All zones, in the API's order (same as [`Zone::ALL`]).
pub const ZONES: [Zone; ZONE_COUNT] = Zone::ALL;

impl Zone {
    /// The two Danish zones, the ones with the tariff-aware endpoints.
    pub const DANISH: [Zone; 2] = [Zone::Dk1, Zone::Dk2];

    /// True for `dk1` and `dk2`.
    pub const fn is_danish(self) -> bool {
        self.area().is_some()
    }

    /// Turn `"de"`, `"DE"`, `" it_n "` into a [`Zone`].
    ///
    /// Whitespace is trimmed, case is ignored and `_` counts as `-`. Fails with
    /// [`Error::UnknownZone`] otherwise (the message lists the valid zones).
    pub fn parse(value: &str) -> Result<Zone> {
        let key = value.trim().to_ascii_lowercase().replace('_', "-");
        Zone::ALL
            .iter()
            .copied()
            .find(|z| z.key() == key)
            .ok_or_else(|| {
                let valid: Vec<&str> = Zone::ALL.iter().map(|z| z.key()).collect();
                Error::UnknownZone(format!(
                    "unknown zone {value:?}; valid zones: {}",
                    valid.join(", ")
                ))
            })
    }
}

impl fmt::Display for Zone {
    /// The API key: `de`, `it-n` ...
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

impl FromStr for Zone {
    type Err = Error;

    fn from_str(s: &str) -> Result<Zone> {
        Zone::parse(s)
    }
}

/// Anything that names a zone: a [`Zone`] or a string (`"de"`, `"IT_N"` ...).
///
/// The client methods take `impl IntoZone`, so both work:
///
/// ```no_run
/// # use elpriser::{Client, Zone};
/// let client = Client::new();
/// client.now(Zone::De)?;
/// client.now("de")?;
/// # Ok::<(), elpriser::Error>(())
/// ```
pub trait IntoZone {
    /// Resolve to a [`Zone`], or [`Error::UnknownZone`].
    fn into_zone(self) -> Result<Zone>;
}

impl IntoZone for Zone {
    fn into_zone(self) -> Result<Zone> {
        Ok(self)
    }
}

impl IntoZone for &Zone {
    fn into_zone(self) -> Result<Zone> {
        Ok(*self)
    }
}

impl IntoZone for &str {
    fn into_zone(self) -> Result<Zone> {
        Zone::parse(self)
    }
}

impl IntoZone for String {
    fn into_zone(self) -> Result<Zone> {
        Zone::parse(&self)
    }
}

impl IntoZone for &String {
    fn into_zone(self) -> Result<Zone> {
        Zone::parse(self)
    }
}
