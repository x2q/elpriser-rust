//! Shared test helpers: recorded API answers, a fake transport and a tiny local HTTP server.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use elpriser::{Response, Transport, TransportError};
use serde_json::{json, Value};

/// A recorded API answer (tests/fixtures, recorded 2026-11-03 from the live API).
pub fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// 2026-11-03 07:20 UTC: 08:20 in Berlin, 07:20 in London.
pub fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 3, 7, 20, 0).unwrap()
}

pub fn ok(payload: &Value) -> Response {
    Response::new(200, serde_json::to_vec(payload).unwrap())
}

pub fn error(status: u16, message: &str) -> Response {
    Response::new(
        status,
        serde_json::to_vec(&json!({ "error": message })).unwrap(),
    )
}

pub fn assert_close(a: f64, b: f64, eps: f64) {
    assert!((a - b).abs() <= eps, "{a} != {b} (eps {eps})");
}

#[derive(Clone, Debug)]
pub struct Call {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
}

type Route = Box<dyn Fn(&str, &BTreeMap<String, String>) -> Response + Send + Sync>;

struct Inner {
    queue: Mutex<VecDeque<Result<Response, TransportError>>>,
    route: Option<Route>,
    calls: Mutex<Vec<Call>>,
}

/// A transport that answers from a queue (the last item repeats) or a route function, and
/// records its calls.
#[derive(Clone)]
pub struct FakeTransport {
    inner: Arc<Inner>,
}

impl FakeTransport {
    pub fn queued(items: Vec<Result<Response, TransportError>>) -> FakeTransport {
        FakeTransport {
            inner: Arc::new(Inner {
                queue: Mutex::new(items.into()),
                route: None,
                calls: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn routed(
        route: impl Fn(&str, &BTreeMap<String, String>) -> Response + Send + Sync + 'static,
    ) -> FakeTransport {
        FakeTransport {
            inner: Arc::new(Inner {
                queue: Mutex::new(VecDeque::new()),
                route: Some(Box::new(route)),
                calls: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Serves the recorded answer that matches a request.
    pub fn fixtures() -> FakeTransport {
        FakeTransport::routed(fixture_route)
    }

    pub fn calls(&self) -> Vec<Call> {
        self.inner.calls.lock().unwrap().clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.url).collect()
    }
}

impl Transport for FakeTransport {
    fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Response, TransportError> {
        self.inner.calls.lock().unwrap().push(Call {
            url: url.to_string(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            timeout,
        });
        if let Some(route) = &self.inner.route {
            return Ok(route(&path(url), &query(url)));
        }
        let mut queue = self.inner.queue.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue[0].clone()
        }
    }
}

pub fn path(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let from_path = &rest[rest.find('/').unwrap_or(rest.len())..];
    from_path.split('?').next().unwrap().to_string()
}

pub fn query(url: &str) -> BTreeMap<String, String> {
    url.split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn fixture_route(path: &str, params: &BTreeMap<String, String>) -> Response {
    if path == "/api/nordic" {
        return ok(&load(&format!("zone_{}", params["zone"].replace('-', "_"))));
    }
    let name = match path {
        "/api/forecast" if params.get("mode").is_some_and(|m| m.starts_with("net_")) => {
            "dk_forecast_net"
        }
        "/api/forecast" => "dk_forecast",
        "/api/now" => "dk_now",
        "/api/prices" => "dk_prices",
        "/api/schedule" => "dk_schedule",
        "/api/shelly/tariff" => "dk_shelly",
        "/api/supplierlookup" => "supplier_ok",
        _ => return error(404, "no such fixture"),
    };
    ok(&load(name))
}

/// `Client` on the recorded answers, plus the transport to inspect.
pub fn fixture_client() -> (elpriser::Client, FakeTransport) {
    let t = FakeTransport::fixtures();
    let client = elpriser::Client::builder()
        .transport(t.clone())
        .backoff(Duration::ZERO)
        .build()
        .unwrap();
    (client, t)
}

/// A series with hand-made hourly EUR/MWh prices (None = unpriced) for one day (+ more days).
pub fn synthetic(
    day_prices: &[Option<f64>],
    date: &str,
    kind: &str,
    zone: elpriser::Zone,
    more: Vec<Value>,
) -> elpriser::Series {
    let rows: Vec<Value> = day_prices
        .iter()
        .enumerate()
        .map(|(h, p)| json!({ "hour": h, "eur_mwh": p }))
        .collect();
    let mut days = vec![json!({ "date": date, "type": kind, "prices": rows })];
    days.extend(more);
    let payload = json!({ "zoneInfo": { "currency": "EUR", "rate": 1.0 }, "days": days });
    elpriser::Series::from_zone_feed_json(zone, &payload).unwrap()
}

pub fn day_json(date: &str, prices: impl Fn(usize) -> f64) -> Value {
    let rows: Vec<Value> = (0..24)
        .map(|h| json!({ "hour": h, "eur_mwh": prices(h) }))
        .collect();
    json!({ "date": date, "type": "actual", "prices": rows })
}

pub fn some(prices: &[f64]) -> Vec<Option<f64>> {
    prices.iter().map(|p| Some(*p)).collect()
}

// -- local HTTP server ----------------------------------------------------------------------

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, payload: &Value) -> Reply {
        Reply {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(payload).unwrap(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }
}

#[derive(Clone, Debug)]
pub struct SeenRequest {
    pub target: String,
    pub headers: HashMap<String, String>,
}

pub struct TestServer {
    pub base_url: String,
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    pub fn start(handler: impl Fn(usize, &SeenRequest) -> Reply + Send + 'static) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, stop2) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let n = {
                    let mut seen = seen2.lock().unwrap();
                    seen.push(request.clone());
                    seen.len()
                };
                let reply = handler(n, &request);
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&reply.body);
                let _ = stream.flush();
            }
        });
        TestServer {
            base_url: format!("http://{addr}"),
            seen,
            stop,
            addr,
            thread: Some(thread),
        }
    }

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<SeenRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.split("\r\n");
    let target = lines.next()?.split(' ').nth(1)?.to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(SeenRequest { target, headers })
}
