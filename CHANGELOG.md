# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.1] - unreleased

### Fixed

- `Zone::Fi` is on Helsinki's clock: elpriser.org now serves `/api/nordic?zone=fi` in Helsinki days (it was
  indexed by the Copenhagen hour), so hour 0 of a Finnish day is 22:00 UTC in winter. 0.1.0 assumed Copenhagen
  time for `fi` and is one hour off once the API has changed; 0.1.1 is one hour off against the old API.
  Publish it together with the API change.

## [0.1.0] - 2026-11-03

First release. Feature parity with the Python SDK
([elpriser-python](https://github.com/x2q/elpriser-python) 0.1.0).

### Added

- `Zone`: all 40 zones of the API with key, label, country, currency, time zone, group and Danish area;
  `Display`, `FromStr`, `Zone::ALL` / `ZONES`.
- Blocking `Client` (ureq with rustls): `series`, `zone_feed`, `now`, `next_hour`, `today`, `tomorrow`,
  `cheapest_hours`, `best_window`, `forecast`, `levels`, each with a `_with(&Query)` twin; the Danish endpoints
  `dk_now`, `dk_prices`, `dk_schedule`, `shelly_tariff`, `supplier_lookup`; `get_json`.
- `Series` with `now`, `next_hour`, `today`, `tomorrow`, `cheapest_hours`, `best_window`, `levels`, `level`,
  `stats`, `latest_known`, `forecast_days`, `settled_days`, accuracy helpers, and `currency` / `unit` / `rate` /
  `rate_date` / `rate_source`.
- Price slots with `PriceKind` (`Actual`, `Forecast`, `Estimate`), P10/P90 on forecast hours, `PriceLevel` quintiles
  (the algorithm of `/api/shelly/tariff`), daylight-saving-correct 23/24/25-hour days.
- `Mode`, `Strategy` and `Charges` enums, parameter validation before any request.
- Timeouts, retries with `Retry-After` (capped at 30 s) and exponential backoff, `User-Agent` `elpriser-rust/<version>`,
  typed `Error` (thiserror).
- Replaceable `Transport` for tests and proxies.
- `AsyncClient` behind the `async` feature (reqwest, tokio).
- `examples/elpriser.rs`, a small command line.
