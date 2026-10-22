//! What the service sends and takes, and how the client talks to it: plain fetches, and an
//! outbox in localStorage so a comment written on a flaky connection is never lost.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub client: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Me {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Version {
    pub id: String,
    pub n: i64,
    #[serde(default)]
    pub label: String,
    pub uploaded: i64,
    pub pages: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub versions: Vec<Version>,
}

/// `GET /api/r/{token}`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SharePage {
    pub project: Project,
    pub mode: String,
    pub allow_download: bool,
    #[serde(default)]
    pub password: bool,
    pub me: Option<Me>,
    pub documents: Vec<Document>,
}

/// `401 {error: "login", mode, project}`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LoginNeeded {
    pub mode: String,
    #[serde(default)]
    pub project: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PageInfo {
    pub index: usize,
    pub label: String,
    pub width: f64,
    pub height: f64,
    pub image: String,
}

/// The bundle's `manifest.json`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Manifest {
    pub title: String,
    pub scale: f64,
    pub pages: Vec<PageInfo>,
    #[serde(default)]
    pub pdf: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Cell {
    pub table: u64,
    pub row: u64,
    pub col: u64,
}

/// A line of the bundle's `text.json`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Line {
    pub story: u64,
    #[serde(default)]
    pub cell: Option<Cell>,
    #[serde(default)]
    pub footnote: Option<u64>,
    pub frame: u64,
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub bytes: Vec<u32>,
    pub rect: [f64; 4],
    #[serde(default)]
    pub rtl: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct PageText {
    pub index: usize,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TextLayer {
    pub pages: Vec<PageText>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
pub struct Author {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub role: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
pub struct Quote {
    #[serde(default)]
    pub exact: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub document: String,
    pub version: String,
    pub kind: String,
    pub page: i64,
    #[serde(default)]
    pub story: Option<u64>,
    #[serde(default)]
    pub cell: Option<Cell>,
    #[serde(default)]
    pub footnote: Option<u64>,
    #[serde(default)]
    pub start: Option<u32>,
    #[serde(default)]
    pub end: Option<u32>,
    #[serde(default)]
    pub quote: Quote,
    #[serde(default)]
    pub rects: Option<Vec<[f64; 4]>>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub replacement: Option<String>,
    pub status: String,
    pub created: i64,
    pub modified: i64,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    pub author: Author,
}

/// What a request came back with.
#[derive(Clone, Debug)]
pub enum Failure {
    /// The service wants a login first.
    Login(LoginNeeded),
    /// The service answered with an error message.
    Refused(u16, String),
    /// No answer: offline, or the service is down.
    Offline,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Login(_) => f.write_str("please sign in"),
            Failure::Refused(status, m) => write!(f, "{m} ({status})"),
            Failure::Offline => f.write_str("no connection; your comment is kept and sent when the connection is back"),
        }
    }
}

async fn answer<T: serde::de::DeserializeOwned>(resp: gloo_net::http::Response) -> Result<T, Failure> {
    if resp.ok() {
        return resp.json::<T>().await.map_err(|e| Failure::Refused(resp.status(), format!("unexpected answer: {e}")));
    }
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status == 401 && body["error"] == "login" {
        return Err(Failure::Login(serde_json::from_value(body).unwrap_or(LoginNeeded { mode: "invite".into(), project: String::new() })));
    }
    Err(Failure::Refused(status, body["error"].as_str().map_or_else(|| format!("the service answered {status}"), str::to_string)))
}

pub async fn get<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, Failure> {
    let resp = gloo_net::http::Request::get(url).send().await.map_err(|_| Failure::Offline)?;
    answer(resp).await
}

pub async fn send<T: serde::de::DeserializeOwned>(method: &str, url: &str, body: &Value) -> Result<T, Failure> {
    let req = match method {
        "PATCH" => gloo_net::http::Request::patch(url),
        _ => gloo_net::http::Request::post(url),
    };
    let resp = req
        .header("content-type", "application/json")
        .body(body.to_string())
        .map_err(|_| Failure::Offline)?
        .send()
        .await
        .map_err(|_| Failure::Offline)?;
    answer(resp).await
}

/// A write that is waiting for a connection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub method: String,
    pub url: String,
    pub body: Value,
}

const OUTBOX: &str = "review-outbox";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn outbox() -> Vec<Pending> {
    storage().and_then(|s| s.get_item(OUTBOX).ok().flatten()).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

fn save_outbox(items: &[Pending]) {
    if let Some(s) = storage() {
        let _ = s.set_item(OUTBOX, &serde_json::to_string(items).unwrap_or_else(|_| "[]".into()));
    }
}

/// Keep a write for later.
pub fn enqueue(p: Pending) {
    let mut items = outbox();
    if !items.contains(&p) {
        items.push(p);
    }
    save_outbox(&items);
}

/// Send what is waiting, in order; stops at the first write that gets no answer. Returns how
/// many were sent.
pub async fn flush() -> usize {
    let mut items = outbox();
    let mut sent = 0;
    while let Some(p) = items.first().cloned() {
        match send::<Value>(&p.method, &p.url, &p.body).await {
            Ok(_) => {
                items.remove(0);
                sent += 1;
            }
            // Refused for good (the comment exists, the link is gone): drop it rather than retry forever.
            Err(Failure::Refused(..)) | Err(Failure::Login(_)) => {
                items.remove(0);
            }
            Err(Failure::Offline) => break,
        }
        save_outbox(&items);
    }
    sent
}

/// A fresh lower-case UUID v4 for a comment the client makes, so a retry stores it once.
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    if let Some(w) = web_sys::window()
        && let Ok(c) = w.crypto()
    {
        let _ = c.get_random_values_with_u8_array(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The share page URL for a token, and the API paths under it.
pub fn api(token: &str) -> String {
    format!("/api/r/{token}")
}

/// The body of `POST …/comments` for a new comment.
#[allow(clippy::too_many_arguments)]
pub fn comment_body(
    id: &str,
    document: &str,
    version: &str,
    kind: &str,
    page: usize,
    anchor: Option<&crate::anchor::Anchor>,
    rects: &[[f64; 4]],
    pin: Option<(f64, f64)>,
    body: &str,
    replacement: Option<&str>,
) -> Value {
    let mut v = json!({"id": id, "document": document, "version": version, "kind": kind, "page": page, "body": body});
    if let Some(a) = anchor {
        v["story"] = json!(a.story);
        v["cell"] = json!(a.cell);
        v["footnote"] = json!(a.footnote);
        v["start"] = json!(a.start);
        v["end"] = json!(a.end);
        v["quote"] = json!({"exact": a.exact, "prefix": a.prefix, "suffix": a.suffix});
        v["rects"] = json!(rects);
    }
    if let Some((x, y)) = pin {
        v["x"] = json!(x);
        v["y"] = json!(y);
    }
    if let Some(r) = replacement {
        v["replacement"] = json!(r);
    }
    v
}
