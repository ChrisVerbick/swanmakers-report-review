//! Time, randomness, hashing, cookies and e-mail addresses.

use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use base64::prelude::*;
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};

/// Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 256 random bits as a URL-safe string: share-link, login and session tokens.
pub fn random_token() -> Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| AppError::Internal(format!("no randomness: {e}")))?;
    Ok(BASE64_URL_SAFE_NO_PAD.encode(b))
}

pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| AppError::Internal(format!("no randomness: {e}")))?;
    Ok(b)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Tokens are stored hashed: a copy of the database gives nobody a session.
pub fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Two secrets compared in time that doesn't depend on where they differ.
pub fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(';'))
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(name).and_then(|r| r.strip_prefix('=')))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

pub fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

pub fn valid_email(e: &str) -> bool {
    e.len() <= 254
        && !e.contains(char::is_whitespace)
        && e.split_once('@').is_some_and(|(u, d)| !u.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.'))
}

/// Text from a client, trimmed and bounded.
pub fn text(s: &str, max_chars: usize, what: &str) -> Result<String> {
    let s = s.trim();
    if s.chars().count() > max_chars {
        return Err(AppError::BadRequest(format!("{what}: at most {max_chars} characters")));
    }
    Ok(s.to_string())
}

/// `secs` as a calendar date, `YYYY-MM-DD` (UTC), for mail.
pub fn date(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_come_out_as_the_calendar_has_them() {
        assert_eq!(super::date(0), "1970-01-01");
        assert_eq!(super::date(1_792_687_873), "2026-10-22");
        assert_eq!(super::date(951_782_400), "2000-02-29");
    }
}
