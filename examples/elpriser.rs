//! A tiny command line for the elpriser.org API: `cargo run --example elpriser -- now --zone DE`.
//!
//! Commands: `now`, `next`, `today`, `tomorrow`, `cheapest`, `window`, `forecast`, `zones`.
//! Options: `-z/--zone ZONE` (default DK1), `-n/--hours N` (default 3), `--mode MODE` and
//! `--gln GLN` (Danish zones), `--settled-only`, `--estimate`, `--json`.
//!
//! Exit status: 0 ok, 1 no price for what you asked, 2 bad usage, 3 API or network error.

use std::process::ExitCode;

use elpriser::{Client, Error, Mode, Query, Series, Slot, Zone};
use serde_json::{json, Value};

struct Args {
    command: String,
    zone: String,
    hours: usize,
    mode: Option<String>,
    gln: Option<String>,
    settled_only: bool,
    estimate: bool,
    json: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        command: String::new(),
        zone: "DK1".into(),
        hours: 3,
        mode: None,
        gln: None,
        settled_only: false,
        estimate: false,
        json: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-z" | "--zone" => args.zone = value("--zone")?,
            "-n" | "--hours" => {
                args.hours = value("--hours")?
                    .parse()
                    .map_err(|_| "--hours must be a number".to_string())?
            }
            "--mode" => args.mode = Some(value("--mode")?),
            "--gln" => args.gln = Some(value("--gln")?),
            "--settled-only" => args.settled_only = true,
            "--estimate" => args.estimate = true,
            "--json" => args.json = true,
            "-h" | "--help" => return Err(String::new()),
            cmd if args.command.is_empty() && !cmd.starts_with('-') => args.command = cmd.into(),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    if args.command.is_empty() {
        return Err(String::new());
    }
    Ok(args)
}

const USAGE: &str = "usage: elpriser <now|next|today|tomorrow|cheapest|window|forecast|zones> \
[-z ZONE] [-n HOURS] [--mode MODE] [--gln GLN] [--settled-only] [--estimate] [--json]";

fn slot_json(slot: &Slot, series: &Series) -> Value {
    json!({
        "start": slot.local_start().to_rfc3339(),
        "end": slot.local_end().to_rfc3339(),
        "price": slot.price,
        "unit": series.price_unit(),
        "kind": slot.kind.as_str(),
        "min": slot.min,
        "max": slot.max,
        "price_mwh": slot.price_mwh,
        "published_unit": series.unit,
    })
}

fn slot_line(slot: &Slot, series: &Series) -> String {
    let price = slot.price.map_or("-".to_string(), |p| format!("{p:.4}"));
    let mut notes = vec![slot.kind.to_string()];
    if let Some(mwh) = slot.price_mwh {
        notes.insert(0, format!("{mwh} {}", series.unit));
    }
    format!(
        "{:<5} {}  {price:>9} {}  ({})",
        series.zone.key().to_uppercase(),
        slot.local_start().format("%Y-%m-%d %H:%M %Z"),
        series.price_unit(),
        notes.join(", ")
    )
}

fn emit(as_json: bool, payload: Value, lines: Vec<String>) {
    if as_json {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("{}", lines.join("\n"));
    }
}

fn run(args: &Args) -> Result<ExitCode, Error> {
    if args.command == "zones" {
        let rows: Vec<Value> = Zone::ALL
            .iter()
            .map(|z| json!({"zone": z.key(), "name": z.label(), "country": z.country(), "currency": z.currency(), "timezone": z.tz().name()}))
            .collect();
        let lines = Zone::ALL
            .iter()
            .map(|z| format!("{:<5} {}  {}", z.key(), z.currency(), z.label()))
            .collect();
        emit(args.json, Value::Array(rows), lines);
        return Ok(ExitCode::SUCCESS);
    }

    let mut query = Query::new()
        .include_forecast(!args.settled_only)
        .include_estimate(args.estimate);
    if let Some(mode) = &args.mode {
        query = query.mode(mode.parse::<Mode>()?);
    }
    if let Some(gln) = &args.gln {
        query = query.gln(gln.clone());
    }
    let series = Client::new().series_with(args.zone.as_str(), &query)?;
    let no_data = |what: &str| {
        eprintln!("elpriser: no {what} in {}", series.zone);
        Ok(ExitCode::from(1))
    };

    match args.command.as_str() {
        "now" | "next" => {
            let slot = if args.command == "now" {
                series.now_with(&query)
            } else {
                series.next_hour_with(&query)
            };
            let Some(slot) = slot else {
                return no_data("price for that hour (try --estimate for gb, pt, ie)");
            };
            emit(
                args.json,
                slot_json(&slot, &series),
                vec![slot_line(&slot, &series)],
            );
        }
        "today" | "tomorrow" => {
            let slots = if args.command == "today" {
                series.today_with(&query)
            } else {
                series.tomorrow_with(&query)
            };
            if slots.is_empty() {
                return no_data(&format!("prices for {}", args.command));
            }
            let rows = slots.iter().map(|s| slot_json(s, &series)).collect();
            emit(
                args.json,
                Value::Array(rows),
                slots.iter().map(|s| slot_line(s, &series)).collect(),
            );
        }
        "cheapest" => {
            let picked = series.cheapest_hours_with(args.hours, &query)?;
            if picked.is_empty() {
                return no_data("priced hours in the next 24 hours");
            }
            let rows = picked.iter().map(|s| slot_json(s, &series)).collect();
            emit(
                args.json,
                Value::Array(rows),
                picked.iter().map(|s| slot_line(s, &series)).collect(),
            );
        }
        "window" => {
            let Some(window) = series.best_window_with(args.hours, &query)? else {
                return no_data(&format!(
                    "{}-hour run of priced hours in the next 24 hours",
                    args.hours
                ));
            };
            let head = format!(
                "{} - {}  avg {:.4} {}",
                window.local_start().format("%Y-%m-%d %H:%M"),
                window.local_end().format("%H:%M %Z"),
                window.average,
                series.price_unit()
            );
            let payload = json!({
                "start": window.local_start().to_rfc3339(),
                "end": window.local_end().to_rfc3339(),
                "hours": window.hours(),
                "average": window.average,
                "unit": series.price_unit(),
                "slots": window.slots.iter().map(|s| slot_json(s, &series)).collect::<Vec<_>>(),
            });
            let lines = std::iter::once(head)
                .chain(
                    window
                        .slots
                        .iter()
                        .map(|s| format!("  {}", slot_line(s, &series))),
                )
                .collect();
            emit(args.json, payload, lines);
        }
        "forecast" => {
            let days = series.forecast_days();
            if days.is_empty() {
                return no_data("forecast days");
            }
            let mut rows = Vec::new();
            let mut lines = Vec::new();
            for day in days {
                let priced: Vec<f64> = day.slots.iter().filter_map(|s| s.price).collect();
                let avg =
                    (!priced.is_empty()).then(|| priced.iter().sum::<f64>() / priced.len() as f64);
                rows.push(json!({"date": day.date.to_string(), "horizon": day.horizon, "average": avg, "unit": series.price_unit()}));
                lines.push(format!(
                    "{}  forecast  {:<4} avg {} {}",
                    day.date,
                    day.horizon.map_or(String::new(), |h| format!("D+{h}")),
                    avg.map_or("-".into(), |a| format!("{a:.4}")),
                    series.price_unit()
                ));
            }
            emit(args.json, Value::Array(rows), lines);
        }
        other => {
            eprintln!("elpriser: unknown command {other:?}\n{USAGE}");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("elpriser: {message}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(code) => code,
        Err(err @ (Error::UnknownZone(_) | Error::InvalidArgument(_))) => {
            eprintln!("elpriser: {err}");
            ExitCode::from(2)
        }
        Err(err) => {
            eprintln!("elpriser: {err}");
            ExitCode::from(3)
        }
    }
}
