//! A review bundle as `review.export` writes it, uploaded as a zip and unpacked into a
//! version's folder. Only the files a bundle may hold are taken, so an archive can't write
//! anywhere else.

use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};
use crate::util;

const MAX_ENTRIES: usize = 2_000;
const MAX_ENTRY: u64 = 64 << 20;
const MAX_TOTAL: u64 = 256 << 20;

pub struct Installed {
    pub app_document_id: String,
    pub title: String,
    pub page_count: i64,
    pub checksum: String,
}

/// A file a bundle may hold: `manifest.json`, `text.json`, `document.pdf`, `pages/NNN.webp`.
pub fn allowed(name: &str) -> bool {
    match name {
        "manifest.json" | "text.json" | "document.pdf" => true,
        _ => name
            .strip_prefix("pages/")
            .and_then(|p| p.strip_suffix(".webp"))
            .is_some_and(|n| (3..=5).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())),
    }
}

/// Unpack `zip` into `dest` (created) and read its manifest.
pub fn install(zip: &[u8], dest: &Path) -> Result<Installed> {
    let bad = |m: String| AppError::BadRequest(format!("review bundle: {m}"));
    let mut z = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| bad(format!("not a zip ({e})")))?;
    if z.len() > MAX_ENTRIES {
        return Err(bad(format!("{} files; at most {MAX_ENTRIES}", z.len())));
    }
    std::fs::create_dir_all(dest.join("pages"))?;
    let (mut total, mut names, mut manifest) = (0u64, BTreeSet::new(), None);
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| bad(format!("entry {i}: {e}")))?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().map_err(|e| bad(format!("entry {i}: {e}")))?.to_string();
        if !allowed(&name) {
            return Err(bad(format!("unexpected file {name}")));
        }
        total = total.saturating_add(f.size());
        if f.size() > MAX_ENTRY || total > MAX_TOTAL {
            return Err(bad("too large".into()));
        }
        let mut buf = Vec::with_capacity(f.size().min(1 << 20) as usize);
        f.read_to_end(&mut buf).map_err(|e| bad(format!("{name}: {e}")))?;
        if name == "manifest.json" {
            manifest = Some(serde_json::from_slice::<Value>(&buf).map_err(|e| bad(format!("manifest.json: {e}")))?);
        }
        std::fs::write(dest.join(&name), &buf)?;
        names.insert(name);
    }
    let m = manifest.ok_or_else(|| bad("no manifest.json".into()))?;
    if m["format"] != "designcraft-review" || m["version"] != 1 {
        return Err(bad("manifest.json: not a designcraft-review bundle, version 1".into()));
    }
    let app_document_id = m["documentId"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 100)
        .ok_or_else(|| bad("manifest.json: documentId".into()))?
        .to_string();
    let pages = m["pages"].as_array().filter(|p| !p.is_empty()).ok_or_else(|| bad("manifest.json: pages".into()))?;
    for p in pages {
        let image = p["image"].as_str().unwrap_or("");
        if !names.contains(image) {
            return Err(bad(format!("page image {image} isn't in the bundle")));
        }
    }
    for needed in ["text.json", m["pdf"].as_str().unwrap_or("document.pdf")] {
        if !names.contains(needed) {
            return Err(bad(format!("{needed} isn't in the bundle")));
        }
    }
    let title = util::text(m["title"].as_str().unwrap_or("Untitled"), 200, "title")?;
    Ok(Installed {
        app_document_id,
        title: if title.is_empty() { "Untitled".into() } else { title },
        page_count: pages.len() as i64,
        checksum: util::hex(&Sha256::digest(zip)),
    })
}
