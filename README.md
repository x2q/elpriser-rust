# elpriser

Rust client for the [elpriser.org](https://elpriser.org) electricity price API:
**40 European price zones**, current and next hour, today and tomorrow hour by hour, cheapest hours,
best window, forecast days with the settled-versus-forecast flag, price levels, and the Danish
tariff-aware endpoints. No API key.

- Blocking `Client` with a small dependency set: [`ureq`](https://crates.io/crates/ureq) (rustls), `chrono`, `chrono-tz`, `serde_json`, `thiserror`.
- Optional `AsyncClient` behind the `async` cargo feature (reqwest + rustls, tokio).
- Timeouts, retries with `Retry-After` and backoff, a proper `User-Agent` (`elpriser-rust/<version>`), typed errors.
- Same features and API shape as the Python SDK, [elpriser-python](https://github.com/x2q/elpriser-python); the
  JavaScript/TypeScript sibling is [elpriser-client](https://github.com/x2q/elpriser-client) and the MCP server for LLM
  clients is [elpriser-mcp](https://github.com/x2q/elpriser-mcp).
- Minimum supported Rust version: **1.85**.

> Not yet on crates.io (the name `elpriser` was free when this was written). Install from GitHub for now:
>
> ```console
> cargo add --git https://github.com/x2q/elpriser-rust
> ```
>
> Once published it will be `cargo add elpriser` (and `cargo add elpriser --features async`).

## Quick start

```rust
use elpriser::{Client, Zone};

fn main() -> Result<(), elpriser::Error> {
    let client = Client::new();

    // The current hour, in local currency per kWh.
    if let Some(slot) = client.now(Zone::De)? {
        println!("{:.4} EUR/kWh at {}", slot.price.unwrap(), slot.local_start()); // 0.3013 EUR/kWh at 2026-11-03 08:00:00 CET
    }

    // The 4 cheapest hours in the next 24 h. `kind` is Actual (settled) or Forecast.
    for slot in client.cheapest_hours("se3", 4)? {
        println!("{} {:?} {}", slot.local_start(), slot.price, slot.kind);
    }

    // The cheapest 3 consecutive hours (EV charging, dishwasher ...).
    if let Some(window) = client.best_window(Zone::Nl, 3)? {
        println!("{} avg {}", window.local_start(), window.average);
    }
    Ok(())
}
```

Every convenience method makes one request. To ask several questions, fetch the series once:

```rust
let s = client.series("pl")?; // one HTTP request
s.now();
s.next_hour();
s.today();
s.tomorrow();
s.cheapest_hours(3);
s.best_window(2)?;
s.levels();        // [(slot, PriceLevel), ...] for today
s.forecast_days(); // only the days that are forecasts
println!("{} {:?} {:?}", s.currency, s.rate, s.rate_date); // PLN Some(4.4185) Some("2026-11-02")
```

Every question has a `_with` twin that takes a `Query`: a fixed moment, a day or a window, settled prices only,
estimates, the Danish price mode.

```rust
use elpriser::Query;

let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
let q = Query::new().day(day).settled_only();
let cheapest = client.cheapest_hours_with("de", 5, &q)?;
let window = client.best_window_with("de", 3, &q)?;
```

### Async

```toml
elpriser = { git = "https://github.com/x2q/elpriser-rust", features = ["async"] }
```

```rust
use elpriser::AsyncClient;

#[tokio::main]
async fn main() -> Result<(), elpriser::Error> {
    let client = AsyncClient::new();
    println!("{:?}", client.now("fr").await?);
    Ok(())
}
```

Same methods as the blocking client, all `async`. `AsyncClient::builder().http_client(reqwest::Client::new())` brings
your own reqwest client.

### Command line

A small command line lives in `examples/` (the crate itself is library-only):

```console
$ cargo run --example elpriser -- now --zone DE
DE    2026-11-03 08:00 CET     0.3013 EUR/kWh  (301.31 EUR/MWh, actual)
$ cargo run --example elpriser -- cheapest -z se3 -n 3 --settled-only
$ cargo run --example elpriser -- window -z dk1 -n 3
$ cargo run --example elpriser -- forecast -z pl
$ cargo run --example elpriser -- today -z gb --estimate --json
$ cargo run --example elpriser -- zones
```

Exit status: 0 ok, 1 no price for what you asked, 2 bad usage, 3 API or network error.

## What the prices are

| Zones | Endpoint | What you get |
|---|---|---|
| `dk1`, `dk2` | `/api/forecast` | **DKK/kWh as a customer pays it**: spot + Energinet's system and transmission tariffs + electricity duty + VAT (`Mode::InklAlt`, the default). Other modes, or `Mode::NetInklAlt` with `.gln(..)` (adds one grid company's tariff), pick a different price. Today plus nine forecast days. |
| the other 38 | `/api/nordic` | **Spot price only**: no VAT, duties or grid fees, so it is not what a consumer pays. Published in EUR/MWh (GBP/MWh for `gb`) and converted by the client to local currency per kWh with the ECB rate the API sends. |

`Slot::price` is always **local currency per kWh** (`series.currency`, e.g. `SEK`). On the zone feed the numbers as
published are kept alongside: `slot.price_mwh` (in `series.unit`, e.g. `EUR/MWh`), `series.rate`, `series.rate_date`,
`series.rate_source`. `price == price_mwh * rate / 1000`. `client.zone_feed("dk1")` gives the plain spot feed for a
Danish zone as well.

### Zones

`Zone` is an enum of all 40 zones (`Zone::De`, `Zone::ItN`, `Zone::Se3` ...) with `.key()`, `.label()`, `.country()`,
`.currency()`, `.tz()` (a `chrono_tz::Tz`), `.group()` and `.area()`. `Zone::ALL` / `ZONES` lists them in the API's
order. It implements `Display` and `FromStr`, and every client method takes a `Zone` or a string (`"de"`, `"DE"`,
`"it-n"`, `"IT_N"`):

```rust
use elpriser::Zone;

let zone: Zone = "it_n".parse()?;
assert_eq!(zone, Zone::ItN);
assert_eq!(zone.to_string(), "it-n");
assert_eq!(zone.currency(), "EUR");
assert_eq!(Zone::Pl.currency(), "PLN");
```

The list is checked against the live `/api/openapi.json` by the opt-in live tests.

### Times

`chrono` with `chrono-tz` handles the zone clocks. `slot.start` / `slot.end` are `DateTime<Utc>` (arithmetic across
daylight saving stays correct); `slot.local_start()` is a `DateTime<Tz>` on the zone's own wall clock, and `slot.hour()`
and `slot.local_date()` are the zone's wall-clock hour and date, which is also what "today" and "tomorrow" mean. A
25-hour day has 25 slots, a 23-hour day 23.

### Settled, forecast, estimate

- `PriceKind::Actual`: the settled day-ahead price.
- `PriceKind::Forecast`: elpriser.org's **own statistical model, not an official forecast**; it is replaced by the
  settled price when that is published (about 13:00 CET for the next day). Forecast hours carry `min` / `max`
  (P10 / P90), and `day.horizon` tells how many days ahead. Per-horizon back-test numbers are in `series.accuracy`
  (`series.accuracy_for(3)`, `series.is_low_confidence(3)`). The model ranks the cheapest hours better than it hits the
  price level. So `tomorrow()` before the auction results are out returns forecast hours: check `slot.kind`, or use
  `Query::new().settled_only()` with `cheapest_hours_with` / `best_window_with` to use settled prices only.
- `PriceKind::Estimate`: for `gb`, `pt` and `ie` the API may attach an `estimate` for hours of a settled day that have
  no price yet (GB: the hours of today that have not traded; PT/IE: the last hour, 23). It is never mixed in unless you
  ask: `Query::new().with_estimate()`.
- **Great Britain is not a day-ahead price**: it is the Market Index Price (within-day), an hour is priced only once it
  has traded, and there is no tomorrow. `series.now()` is therefore `None` for `gb` unless you ask for the estimate;
  `series.latest_known()` returns the newest priced hour.

### Price levels

`PriceLevel` (`VeryCheap` ... `VeryExpensive`) ranks an hour among the priced hours of its local day in quintiles, the
same scale and algorithm as the `level` field of `/api/shelly/tariff`. Use `series.levels()`, `series.level(&slot)` or
`price_levels(&[...])`. `client.shelly_tariff("dk1")` returns the API's own levels.

### Cheapest hours and best window

```rust
s.cheapest_hours(5);                         // the 5 cheapest hours, in time order, anywhere in the next 24 h
s.best_window(5)?;                           // the cheapest 5 consecutive hours
s.best_window_with(3, &Query::new().day(day).settled_only())?;
s.cheapest_hours_with(2, &Query::new().between(t0, t1))?; // any window; default is the next 24 hours
```

Hours without a price never count; fewer than `n` hours come back if fewer are priced. Ties go to the earliest hour.

## Denmark-only endpoints (parity with the npm client)

| npm `elpriser-client` | this crate |
|---|---|
| `now()` | `client.dk_now_with(area, &DkQuery)` (or `client.now("dk1")` for the `Slot`) |
| `prices()` | `client.dk_prices_with(area, &DkQuery::new().date(..).mode(..).gln(..).charges(..))` |
| `schedule()` | `client.dk_schedule_with(area, &DkQuery::new().strategy(..).hours(..).pct(..).max_off(..).date(..))` |
| `forecast()` | `client.forecast("dk1")` / `client.series("dk1")?.forecast_days()` |
| `shellyTariff()` | `client.shelly_tariff(area)` |
| `supplierLookup()` | `client.supplier_lookup(lat, lng)` |

`Mode`, `Strategy` and `Charges` list the values the API accepts. Invalid combinations (a `net_` mode without a GLN, a
`mode` on a non-Danish zone, hours out of range) return `Error::InvalidArgument` before any request.

## Errors, timeouts, retries

```rust
use std::time::Duration;

let client = elpriser::Client::builder()
    .timeout(Duration::from_secs(10))   // per attempt
    .retries(2)
    .backoff(Duration::from_millis(500))
    .user_agent("my-app/1.0")
    .base_url("https://elpriser.org")
    .build()?;
```

Timeouts, connection errors, 429 and 5xx are retried `retries` times (the server's `Retry-After` is honoured, capped at
30 s; otherwise `backoff * 2^attempt`). Other 4xx are not retried. Everything returned is an `elpriser::Error`:

| `Error` variant | When |
|---|---|
| `UnknownZone` | the zone is not one of the 40 |
| `InvalidArgument` | a parameter was rejected before any request |
| `BadRequest` / `NoData` | HTTP 400 / 404 (`.message` carries the API's own text) |
| `RateLimited` / `Server` | HTTP 429 / 5xx after the retries (`.retry_after`) |
| `Http` | any other HTTP error status |
| `Timeout`, `Connection` | no HTTP answer |
| `Parse` | a 200 that is not the shape this version understands |

```rust
match client.now("de") {
    Ok(Some(slot)) => println!("{:?}", slot.price),
    Ok(None) => println!("no price for this hour yet"),
    Err(elpriser::Error::NoData(e)) => println!("the API has no data: {}", e.message),
    Err(e) if e.is_network() => println!("offline? {e}"),
    Err(e) => return Err(e.into()),
}
```

The API's CDN rejects some default agents, so the client always sends its own; if you set `user_agent(..)`, use something
like `"my-app/1.0"`.

For tests, replace the HTTP layer: `Client::builder().transport(..)` takes anything that implements `Transport`
(a closure `|url, headers, timeout| -> Result<Response, TransportError>` will do). `Series::from_zone_feed_json` and
the functions in `elpriser::parse` read a recorded answer without any client.

## Data and attribution

Prices come from elpriser.org, which aggregates: Energi Data Service (Energinet) for Denmark; Bundesnetzagentur |
SMARD.de via Energy-Charts.info (CC BY 4.0) for DE-LU, FR, PL, IT-North, AT, BE, CH, CZ, HU and SI; the ENTSO-E
Transparency Platform for ES, PT, SK, GR, RO, BG, HR, RS, EE, LV, LT, ME, MK, AL, XK and the Nordic/NL zones; SEMOpx for
Ireland; and Elexon BMRS for Great Britain (*Contains BMRS data (c) Elexon Limited copyright and database right*).
Sub-hourly prices are averaged to hourly means by elpriser.org. Keep the attribution if you republish the data.
See <https://elpriser.org/api> and `/api/openapi.json`. The service is free; please keep request volume reasonable
(responses are edge-cached for minutes, so polling more often than every few minutes gains nothing).

## Development

```console
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features                # offline, against recorded API answers in tests/fixtures
cargo test --test live -- --ignored      # opt-in: checks the zone table and the parsers against the live API
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

No test touches the network unless you pass `--ignored`. The default HTTP transport is tested against a throwaway
HTTP server on localhost.

### Maintainers: publishing

`publish.yml` publishes to crates.io when a `v*` tag is pushed, using
[crates.io trusted publishing](https://crates.io/docs/trusted-publishing) (`rust-lang/crates-io-auth-action`, no
token stored in the repo). It is written but **inert** until the owner sets it up:

1. Publish the first release by hand with `cargo publish` (crates.io needs the crate to exist before a trusted
   publisher can be attached to it, as far as we know; the name `elpriser` was free on 2026-11-03).
2. On crates.io open the crate -> Settings -> Trusted Publishing -> add GitHub: owner `x2q`, repository
   `elpriser-rust`, workflow `publish.yml`.
3. In the GitHub repository set the variable `PUBLISH_TO_CRATES_IO` to `true` (Settings -> Secrets and variables ->
   Actions -> Variables). Without it the workflow only verifies the tag and does a `cargo publish --dry-run`.
4. Bump `version` in `Cargo.toml`, update `CHANGELOG.md`, tag `vX.Y.Z` and push the tag.

## Dansk

Rust-klient til elpriser.org: spotpriser for 40 europæiske prisområder, aktuel og næste time, billigste timer, bedste
sammenhængende vindue og prognose. For `dk1`/`dk2` er prisen i kr/kWh inkl. Energinets tariffer, elafgift og moms
(`Mode::InklAlt`); tilføj `Query::new().mode(Mode::NetInklAlt).gln("...")` for dit netselskabs tariff. Uden for Danmark
er det børsprisen uden moms, afgifter og nettariffer. Prognosedage er elpriser.orgs egen model (ikke en officiel
prognose) og markeres med `kind == PriceKind::Forecast`. Hurtig prøve: `cargo run --example elpriser -- now --zone DK1`.

## License

MIT. Data © their respective sources, see above.
