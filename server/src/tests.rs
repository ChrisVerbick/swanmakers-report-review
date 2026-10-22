//! The whole reviewer and owner surface, walked over HTTP against a temporary data folder.

use std::io::Write;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{AppState, mail::Mailer, router};

const ORIGIN: &str = "https://review.test";
const ADMIN: &str = "test-admin-token";

fn state(name: &str) -> AppState {
    let dir = std::env::temp_dir().join(format!("dcr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(&dir, ORIGIN.into(), ADMIN.into(), Mailer::log(), false).unwrap()
}

async fn call(app: &Router, method: Method, path: &str, headers: &[(&str, &str)], body: Body) -> (StatusCode, HeaderMap, Bytes) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status, parts.headers, bytes)
}

async fn json(app: &Router, method: Method, path: &str, headers: &[(&str, &str)], body: Value) -> (StatusCode, HeaderMap, Value) {
    let mut h = vec![("content-type", "application/json")];
    h.extend_from_slice(headers);
    let (s, hs, b) = call(app, method, path, &h, Body::from(body.to_string())).await;
    let v = if b.is_empty() { Value::Null } else { serde_json::from_slice(&b).unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&b)})) };
    (s, hs, v)
}

fn admin(json_body: bool) -> Vec<(&'static str, &'static str)> {
    let mut h = vec![("authorization", "Bearer test-admin-token")];
    if json_body {
        h.push(("content-type", "application/json"));
    }
    h
}

/// A review bundle as `review.export` writes it, zipped.
fn bundle_zip(document_id: &str, title: &str) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    let manifest = json!({
        "format": "designcraft-review", "version": 1, "generator": "test",
        "documentId": document_id, "versionId": 1, "title": title, "scale": 2.0,
        "pages": [{"index": 0, "label": "1", "spread": 0, "width": 612.0, "height": 792.0, "image": "pages/001.webp"}],
        "pdf": "document.pdf", "text": "text.json", "warnings": []
    });
    z.start_file("manifest.json", o).unwrap();
    z.write_all(manifest.to_string().as_bytes()).unwrap();
    z.start_file("text.json", o).unwrap();
    z.write_all(json!({"pages": [{"index": 0, "lines": [{"story": 3, "frame": 7, "start": 0, "end": 5, "text": "Hello", "bytes": [0,1,2,3,4], "rect": [72.0, 72.0, 110.0, 84.0]}]}]}).to_string().as_bytes()).unwrap();
    z.start_file("pages/001.webp", o).unwrap();
    z.write_all(b"RIFF\x10\x00\x00\x00WEBPVP8L").unwrap();
    z.start_file("document.pdf", o).unwrap();
    z.write_all(b"%PDF-1.4 test").unwrap();
    z.finish().unwrap().into_inner()
}

fn cookie_of(headers: &HeaderMap) -> String {
    let set = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    assert!(
        set.starts_with("__Host-sid=")
            && set.contains("HttpOnly")
            && set.contains("Secure")
            && set.contains("SameSite=Lax")
            && set.contains("Path=/"),
        "{set}"
    );
    set.split(';').next().unwrap().to_string()
}

/// Owner side: a project, a reviewer, an invite-only link and one uploaded version.
async fn project_with_version(app: &Router) -> (String, String, String, String) {
    let (s, _, p) = json(app, Method::POST, "/api/projects", &admin(false), json!({"name": "Annual report", "client": "Acme"})).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let project = p["id"].as_str().unwrap().to_string();
    let (s, _, m) = json(
        app,
        Method::POST,
        &format!("/api/projects/{project}/members"),
        &admin(false),
        json!({"email": "Ann@Acme.example", "name": "Ann", "role": "reviewer"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert_eq!(m["email"], "ann@acme.example", "emails are normalised");
    let (s, _, l) = json(app, Method::POST, &format!("/api/projects/{project}/share-links"), &admin(false), json!({"mode": "invite"})).await;
    assert_eq!(s, StatusCode::CREATED, "{l}");
    let token = l["token"].as_str().unwrap().to_string();
    assert_eq!(l["url"], format!("{ORIGIN}/r/{token}"));
    let (s, _, v) = call(
        app,
        Method::POST,
        &format!("/api/projects/{project}/versions"),
        &[("authorization", "Bearer test-admin-token"), ("content-type", "application/zip")],
        Body::from(bundle_zip("doc-1", "Annual report 2026")),
    )
    .await;
    let v: Value = serde_json::from_slice(&v).unwrap();
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!((v["version"]["n"].as_i64(), v["document"]["appDocumentId"].as_str()), (Some(1), Some("doc-1")));
    (project, token, v["document"]["id"].as_str().unwrap().to_string(), v["version"]["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn the_owner_api_needs_the_admin_token() {
    let app = router(state("admin"));
    let (s, ..) = json(&app, Method::POST, "/api/projects", &[], json!({"name": "x"})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, ..) = json(&app, Method::POST, "/api/projects", &[("authorization", "Bearer wrong")], json!({"name": "x"})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_document_is_bound_to_one_project() {
    let app = router(state("bind"));
    let (project, ..) = project_with_version(&app).await;
    let (s, _, other) = json(&app, Method::POST, "/api/projects", &admin(false), json!({"name": "Other client"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let other = other["id"].as_str().unwrap();
    let (s, _, b) = call(
        &app,
        Method::POST,
        &format!("/api/projects/{other}/versions"),
        &[("authorization", "Bearer test-admin-token")],
        Body::from(bundle_zip("doc-1", "Annual report 2026")),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(String::from_utf8_lossy(&b).contains("Annual report"), "names the bound project: {}", String::from_utf8_lossy(&b));
    // The same document in its own project is a second version.
    let (s, _, v) = call(
        &app,
        Method::POST,
        &format!("/api/projects/{project}/versions"),
        &[("authorization", "Bearer test-admin-token")],
        Body::from(bundle_zip("doc-1", "Annual report 2026")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(serde_json::from_slice::<Value>(&v).unwrap()["version"]["n"], 2);
}

#[tokio::test]
async fn a_bad_bundle_is_refused() {
    let app = router(state("badzip"));
    let (s, _, p) = json(&app, Method::POST, "/api/projects", &admin(false), json!({"name": "P"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let project = p["id"].as_str().unwrap();
    let (s, ..) = call(
        &app,
        Method::POST,
        &format!("/api/projects/{project}/versions"),
        &[("authorization", "Bearer test-admin-token")],
        Body::from("not a zip"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file("../escape.txt", zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(b"x").unwrap();
    let (s, ..) = call(
        &app,
        Method::POST,
        &format!("/api/projects/{project}/versions"),
        &[("authorization", "Bearer test-admin-token")],
        Body::from(z.finish().unwrap().into_inner()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_invited_reviewer_logs_in_by_magic_link_reads_the_bundle_and_comments() {
    let st = state("walk");
    let app = router(st.clone());
    let (project, token, document, version) = project_with_version(&app).await;
    let r = format!("/api/r/{token}");

    // The share page asks an invitee to log in; a stranger's email gets the same answer as a member's.
    let (s, _, v) = json(&app, Method::GET, &r, &[], Value::Null).await;
    assert_eq!((s, v["error"].as_str(), v["mode"].as_str()), (StatusCode::UNAUTHORIZED, Some("login"), Some("invite")));
    let (s, ..) = json(&app, Method::POST, "/api/auth/request", &[], json!({"email": "nobody@else.example", "link": token})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(st.mailer.outbox().is_empty(), "no mail to a stranger");
    let (s, ..) = json(&app, Method::POST, "/api/auth/request", &[], json!({"email": "ANN@acme.example", "link": token})).await;
    assert_eq!(s, StatusCode::OK);
    let mail = st.mailer.outbox().pop().expect("a magic link mail");
    assert_eq!(mail.to, "ann@acme.example");
    let url = mail.body.lines().find(|l| l.starts_with(ORIGIN)).expect("the link in the mail").to_string();
    let path = url.strip_prefix(ORIGIN).unwrap().to_string();
    assert!(path.starts_with("/auth?token="), "{path}");

    // The landing page is a form (mail scanners that GET the link can't consume it).
    let (s, h, body) = call(&app, Method::GET, &path, &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with("text/html"));
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<form") && html.contains("method=\"post\"") && html.contains("name=\"token\""), "{html}");
    let login = path.strip_prefix("/auth?token=").unwrap().split('&').next().unwrap().to_string();
    let form = format!("token={login}&to=%2Fr%2F{token}");
    let (s, h, _) = call(
        &app,
        Method::POST,
        "/api/auth/consume",
        &[("content-type", "application/x-www-form-urlencoded"), ("origin", ORIGIN)],
        Body::from(form.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h.get("location").unwrap(), &format!("/r/{token}"));
    let cookie = cookie_of(&h);
    let (s, ..) =
        call(&app, Method::POST, "/api/auth/consume", &[("content-type", "application/x-www-form-urlencoded"), ("origin", ORIGIN)], Body::from(form))
            .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "a magic link works once");

    // The share page, the bundle files.
    let auth = [("cookie", cookie.as_str())];
    let (s, _, page) = json(&app, Method::GET, &r, &auth, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(
        (page["project"]["name"].as_str(), page["me"]["email"].as_str(), page["me"]["role"].as_str()),
        (Some("Annual report"), Some("ann@acme.example"), Some("reviewer"))
    );
    assert_eq!(page["documents"][0]["versions"][0]["pages"], 1);
    assert_eq!(page["documents"][0]["versions"][0]["id"], version);
    for (file, ct) in [
        ("manifest.json", "application/json"),
        ("text.json", "application/json"),
        ("pages/001.webp", "image/webp"),
        ("document.pdf", "application/pdf"),
    ] {
        let (s, h, b) = call(&app, Method::GET, &format!("{r}/v/{version}/{file}"), &auth, Body::empty()).await;
        assert_eq!(s, StatusCode::OK, "{file}");
        assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with(ct), "{file}");
        assert!(!b.is_empty());
    }
    let (s, ..) = call(&app, Method::GET, &format!("{r}/v/{version}/../../db.sqlite"), &auth, Body::empty()).await;
    assert_ne!(s, StatusCode::OK);
    let (s, ..) = call(&app, Method::GET, &format!("{r}/v/{version}/pages/001.webp"), &[], Body::empty()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "files need the login too");

    // A comment: a cross-site origin is refused, the real one accepted; then a reply and a resolve.
    let comment = json!({"document": document, "version": version, "kind": "replace", "page": 0, "story": 3, "start": 0, "end": 5,
        "quote": {"exact": "Hello", "prefix": "", "suffix": " world"}, "rects": [[72.0, 72.0, 110.0, 84.0]], "body": "Say hi", "replacement": "Hi"});
    let (s, ..) = json(&app, Method::POST, &format!("{r}/comments"), &[("cookie", &cookie), ("origin", "https://evil.test")], comment.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, c) = json(&app, Method::POST, &format!("{r}/comments"), &[("cookie", &cookie), ("origin", ORIGIN)], comment).await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let id = c["id"].as_str().unwrap().to_string();
    assert_eq!((c["author"]["name"].as_str(), c["status"].as_str(), c["replacement"].as_str()), (Some("Ann"), Some("open"), Some("Hi")));
    let (s, _, reply) =
        json(&app, Method::POST, &format!("{r}/comments/{id}/replies"), &[("cookie", &cookie), ("origin", ORIGIN)], json!({"body": "Agreed"})).await;
    assert_eq!(s, StatusCode::CREATED, "{reply}");
    assert_eq!(reply["inReplyTo"], id);
    let (s, _, c) =
        json(&app, Method::PATCH, &format!("{r}/comments/{id}"), &[("cookie", &cookie), ("origin", ORIGIN)], json!({"status": "resolved"})).await;
    assert_eq!((s, c["status"].as_str()), (StatusCode::OK, Some("resolved")));
    let (s, _, list) = json(&app, Method::GET, &format!("{r}/comments?document={document}"), &auth, Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}");

    // Events stream from the start: the backlog carries the comment, the reply and the resolve.
    let req = Request::builder().method(Method::GET).uri(format!("{r}/events?since=0")).header("cookie", &cookie).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("content-type").unwrap().to_str().unwrap().starts_with("text/event-stream"));
    let mut body = resp.into_body();
    let mut text = String::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !(text.contains("comment.created") && text.contains("comment.replied") && text.contains("comment.updated")) {
        assert!(std::time::Instant::now() < deadline, "events so far: {text}");
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame()).await.unwrap().unwrap().unwrap();
        if let Some(data) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(data));
        }
    }
    assert!(text.contains("id: "), "events carry ids for Last-Event-ID: {text}");

    // The owner pulls everything with its anchors; the audit log has the login and the comment.
    let (s, _, all) = json(&app, Method::GET, &format!("/api/projects/{project}/comments.json"), &admin(false), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let first = all["comments"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap();
    assert_eq!(
        (first["story"].as_i64(), first["start"].as_i64(), first["end"].as_i64(), first["quote"]["exact"].as_str()),
        (Some(3), Some(0), Some(5), Some("Hello"))
    );
    assert_eq!(first["status"], "resolved");
    let (s, _, detail) = json(&app, Method::GET, &format!("/api/projects/{project}"), &admin(false), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let kinds: Vec<&str> = detail["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(kinds.contains(&"login.succeeded") && kinds.contains(&"comment.created"), "{kinds:?}");
    // The app marks a comment applied.
    let (s, _, c) = json(&app, Method::PATCH, &format!("/api/comments/{id}"), &admin(false), json!({"status": "applied"})).await;
    assert_eq!((s, c["status"].as_str()), (StatusCode::OK, Some("applied")));

    // Logout ends the session; a revoked link is gone for everyone.
    let (s, ..) = json(&app, Method::POST, "/api/auth/logout", &[("cookie", &cookie), ("origin", ORIGIN)], Value::Null).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, ..) = json(&app, Method::GET, &r, &auth, Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let link_id = detail["links"][0]["id"].as_str().unwrap();
    let (s, ..) = json(&app, Method::DELETE, &format!("/api/share-links/{link_id}"), &admin(false), Value::Null).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, ..) = json(&app, Method::GET, &r, &[], Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn login_requests_are_rate_limited() {
    let app = router(state("rate"));
    let (_, token, ..) = project_with_version(&app).await;
    let mut last = StatusCode::OK;
    for _ in 0..8 {
        let (s, ..) = json(&app, Method::POST, "/api/auth/request", &[], json!({"email": "ann@acme.example", "link": token})).await;
        last = s;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_public_link_takes_guests_by_name_and_password() {
    let app = router(state("public"));
    let (project, _, document, version) = project_with_version(&app).await;
    let (s, _, l) =
        json(&app, Method::POST, &format!("/api/projects/{project}/share-links"), &admin(false), json!({"mode": "public", "password": "spring"}))
            .await;
    assert_eq!(s, StatusCode::CREATED, "{l}");
    let token = l["token"].as_str().unwrap();
    let r = format!("/api/r/{token}");
    // Anyone with the link may look; commenting needs a name and the password.
    let (s, _, page) = json(&app, Method::GET, &r, &[], Value::Null).await;
    assert_eq!((s, page["me"].is_null(), page["mode"].as_str()), (StatusCode::OK, true, Some("public")));
    let (s, ..) =
        json(&app, Method::POST, "/api/auth/guest", &[], json!({"link": token, "name": "Bob", "email": "bob@partner.example", "password": "wrong"}))
            .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, h, _) =
        json(&app, Method::POST, "/api/auth/guest", &[], json!({"link": token, "name": "Bob", "email": "bob@partner.example", "password": "spring"}))
            .await;
    assert_eq!(s, StatusCode::OK);
    let cookie = cookie_of(&h);
    let (s, _, page) = json(&app, Method::GET, &r, &[("cookie", &cookie)], Value::Null).await;
    assert_eq!((s, page["me"]["name"].as_str(), page["me"]["role"].as_str()), (StatusCode::OK, Some("Bob"), Some("guest")));
    let pin = json!({"document": document, "version": version, "kind": "pin", "page": 0, "x": 100.0, "y": 200.0, "body": "Logo is blurry"});
    let (s, _, c) = json(&app, Method::POST, &format!("{r}/comments"), &[("cookie", &cookie), ("origin", ORIGIN)], pin).await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let id = c["id"].as_str().unwrap();
    let (s, ..) =
        json(&app, Method::PATCH, &format!("{r}/comments/{id}"), &[("cookie", &cookie), ("origin", ORIGIN)], json!({"status": "resolved"})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "guests don't resolve");
    // Downloads can be switched off per project; the pages stay.
    let (s, ..) = json(&app, Method::PATCH, &format!("/api/projects/{project}"), &admin(false), json!({"allowDownload": false})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, ..) = call(&app, Method::GET, &format!("{r}/v/{version}/document.pdf"), &[("cookie", &cookie)], Body::empty()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, ..) = call(&app, Method::GET, &format!("{r}/v/{version}/pages/001.webp"), &[("cookie", &cookie)], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let app = router(state("headers"));
    let (s, h, _) = call(&app, Method::GET, "/", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
}

#[tokio::test]
async fn the_client_is_served_under_a_hashed_csp_and_api_misses_stay_json() {
    let web = std::env::temp_dir().join(format!("dcr-client-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&web);
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html><html><head><script type=\"module\">console.log('hi')</script><script src=\"/app.js\"></script></head><body>the app</body></html>").unwrap();
    std::fs::write(web.join("assets/app.css"), "body{}").unwrap();
    let app = router(state("web").with_web_dir(&web).unwrap());
    let (s, h, b) = call(&app, Method::GET, "/r/sometoken", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK, "a client route serves the app's page; got {}", String::from_utf8_lossy(&b));
    assert!(String::from_utf8_lossy(&b).contains("the app"));
    let csp = h.get("content-security-policy").unwrap().to_str().unwrap();
    use base64::prelude::*;
    use sha2::Digest;
    let hash = format!("'sha256-{}'", BASE64_STANDARD.encode(sha2::Sha256::digest("console.log('hi')")));
    assert!(csp.contains(&format!("script-src 'self' 'wasm-unsafe-eval' {hash}")), "{csp}");
    assert!(csp.contains("style-src 'self'") && !csp.contains("unsafe-inline"), "{csp}");
    let (s, h, _) = call(&app, Method::GET, "/assets/app.css", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with("text/css"));
    let (s, h, _) = call(&app, Method::GET, "/api/nothing-here", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(h.get("content-type").unwrap().to_str().unwrap().starts_with("application/json"), "API misses stay JSON");
    let app = router(state("noweb"));
    let (s, ..) = call(&app, Method::GET, "/r/sometoken", &[], Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "without a web folder there is no page");
}

#[tokio::test]
async fn a_public_share_page_says_whether_a_password_is_needed() {
    let app = router(state("pwflag"));
    let (project, ..) = project_with_version(&app).await;
    for (password, expect) in [(json!(null), false), (json!("spring"), true)] {
        let (s, _, l) =
            json(&app, Method::POST, &format!("/api/projects/{project}/share-links"), &admin(false), json!({"mode": "public", "password": password}))
                .await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, _, page) = json(&app, Method::GET, &format!("/api/r/{}", l["token"].as_str().unwrap()), &[], Value::Null).await;
        assert_eq!((s, page["password"].as_bool()), (StatusCode::OK, Some(expect)), "{page}");
    }
}

#[tokio::test]
async fn a_comment_posted_twice_with_its_own_id_is_stored_once() {
    let app = router(state("idem"));
    let (project, _, document, version) = project_with_version(&app).await;
    let (s, _, l) = json(&app, Method::POST, &format!("/api/projects/{project}/share-links"), &admin(false), json!({"mode": "public"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let token = l["token"].as_str().unwrap();
    let (_, h, _) = json(&app, Method::POST, "/api/auth/guest", &[], json!({"link": token, "name": "Bob", "email": "bob@partner.example"})).await;
    let cookie = cookie_of(&h);
    let r = format!("/api/r/{token}");
    let id = "0b1f6d6e-9c5b-4f2e-8a21-3f6e1b2c9d10";
    let pin = json!({"id": id, "document": document, "version": version, "kind": "pin", "page": 0, "x": 1.0, "y": 2.0, "body": "once"});
    for _ in 0..2 {
        let (s, _, c) = json(&app, Method::POST, &format!("{r}/comments"), &[("cookie", &cookie), ("origin", ORIGIN)], pin.clone()).await;
        assert_eq!((s, c["id"].as_str()), (StatusCode::CREATED, Some(id)), "{c}");
    }
    let reply_id = "7d3a2b1c-4e5f-4a6b-9c8d-0e1f2a3b4c5d";
    for _ in 0..2 {
        let (s, _, c) = json(
            &app,
            Method::POST,
            &format!("{r}/comments/{id}/replies"),
            &[("cookie", &cookie), ("origin", ORIGIN)],
            json!({"id": reply_id, "body": "same"}),
        )
        .await;
        assert_eq!((s, c["id"].as_str()), (StatusCode::CREATED, Some(reply_id)), "{c}");
    }
    let (_, _, list) = json(&app, Method::GET, &format!("{r}/comments?document={document}"), &[("cookie", &cookie)], Value::Null).await;
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}");
    let (s, ..) = json(
        &app,
        Method::POST,
        &format!("{r}/comments"),
        &[("cookie", &cookie), ("origin", ORIGIN)],
        json!({"id": "not-a-uuid", "document": document, "version": version, "kind": "pin", "page": 0, "x": 1.0, "y": 2.0}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_app_finds_a_document_s_project_by_its_own_id_and_replies_as_the_owner() {
    let app = router(state("binding"));
    let (s, ..) = json(&app, Method::GET, "/api/documents/doc-1", &admin(false), Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nothing published yet");
    let (project, _, document, version) = project_with_version(&app).await;
    let (s, _, b) = json(&app, Method::GET, "/api/documents/doc-1", &admin(false), Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        (b["project"]["id"].as_str(), b["project"]["name"].as_str(), b["document"]["id"].as_str()),
        (Some(project.as_str()), Some("Annual report"), Some(document.as_str()))
    );
    assert_eq!(b["versions"][0]["id"], version);
    // A guest comments; the owner replies through the admin API, as "Chris".
    let (s, _, l) = json(&app, Method::POST, &format!("/api/projects/{project}/share-links"), &admin(false), json!({"mode": "public"})).await;
    assert_eq!(s, StatusCode::CREATED);
    let link = l["token"].as_str().unwrap();
    let (_, h, _) = json(&app, Method::POST, "/api/auth/guest", &[], json!({"link": link, "name": "Bob", "email": "bob@partner.example"})).await;
    let cookie = cookie_of(&h);
    let (s, _, c) = json(
        &app,
        Method::POST,
        &format!("/api/r/{link}/comments"),
        &[("cookie", &cookie), ("origin", ORIGIN)],
        json!({"document": document, "version": version, "kind": "note", "page": 0, "body": "Hi"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let id = c["id"].as_str().unwrap();
    let (s, _, r) =
        json(&app, Method::POST, &format!("/api/comments/{id}/replies"), &admin(false), json!({"body": "Will do", "name": "Chris"})).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!((r["inReplyTo"].as_str(), r["author"]["name"].as_str(), r["author"]["role"].as_str()), (Some(id), Some("Chris"), Some("owner")));
    let (_, _, list) = json(&app, Method::GET, &format!("/api/r/{link}/comments?document={document}"), &[("cookie", &cookie)], Value::Null).await;
    assert_eq!(list.as_array().unwrap().len(), 2, "the reviewer sees the owner's reply: {list}");
    let (s, _, again) = json(&app, Method::POST, &format!("/api/comments/{id}/replies"), &admin(false), json!({"body": "And again"})).await;
    assert_eq!((s, again["author"]["name"].as_str()), (StatusCode::CREATED, Some("Chris")), "the owner member is reused");
    let (s, ..) = json(&app, Method::POST, "/api/comments/nope/replies", &admin(false), json!({"body": "x"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
