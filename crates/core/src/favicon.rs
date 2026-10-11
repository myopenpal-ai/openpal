//! Best-effort favicon fetching for web-search sources.
//!
//! The webview has no network access and our CSP forbids remote images
//! (`img-src 'self' data:`), so a source's site icon can't be shown with a
//! plain `<img src="https://…">`. Instead the frontend asks the backend for
//! one and we hand back a `data:` URI it can render directly.
//!
//! We only ever fetch what the page itself points at — its declared
//! `<link rel="icon">` (which, like in a browser, may live on the site's CDN)
//! or its `/favicon.ico` — never a third-party favicon/tracking service of our
//! choosing. And because the URLs ultimately come from web-search results
//! (attacker-influenceable), every request is gated to public hosts: the fetch
//! refuses localhost and private/loopback/link-local addresses, directly or via
//! a redirect, so it can't be steered at internal services.

use base64::Engine;
use futures_util::StreamExt;
use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;

/// Hard caps so a hostile or broken server can't make us hang or balloon memory.
const MAX_ICON_BYTES: usize = 512 * 1024;
const MAX_HTML_BYTES: usize = 256 * 1024;
/// Most icon URLs a page can offer before we stop trying (declared icons,
/// best-first, plus the /favicon.ico fallback).
const MAX_CANDIDATES: usize = 8;
const MAX_REDIRECTS: usize = 5;
/// Per network read (connect/headers, then each body chunk).
const READ_TIMEOUT: Duration = Duration::from_secs(8);
/// Overall budget for one `fetch` so a chain of slow candidates can't add up.
const TOTAL_BUDGET: Duration = Duration::from_secs(12);

/// A dedicated HTTP client for favicons. Separate from the app's shared client
/// so we can install a redirect policy (refuse redirects onto non-public hosts)
/// without affecting downloads / Hugging Face, which legitimately redirect
/// across CDN hosts.
fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("OpenPal/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(8))
                .read_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= MAX_REDIRECTS {
                        attempt.error("too many redirects")
                    } else if host_is_blocked_url(attempt.url()) {
                        attempt.error("redirect to a non-public host blocked")
                    } else {
                        attempt.follow()
                    }
                }))
                .build()
                .ok()
        })
        .as_ref()
}

/// Resolve `page_url`'s favicon as a `data:` URI (e.g. `data:image/png;base64,…`),
/// or `None` if no usable icon could be found.
///
/// Never errors: a missing icon is normal and simply means "show the fallback".
pub async fn fetch(page_url: &str) -> Option<String> {
    // The UI never waits on this (it shows a globe until an icon arrives), but
    // we still bound total work so a string of slow candidates can't drag on.
    tokio::time::timeout(TOTAL_BUDGET, fetch_inner(page_url))
        .await
        .ok()
        .flatten()
}

async fn fetch_inner(page_url: &str) -> Option<String> {
    let client = client()?;
    let page = reqwest::Url::parse(page_url).ok()?;
    if !matches!(page.scheme(), "http" | "https") || host_is_blocked_url(&page) {
        return None;
    }

    // Prefer icons the page declares in <head>; fall back to the well-known
    // /favicon.ico that every browser tries by default.
    let mut candidates: Vec<reqwest::Url> = Vec::new();
    if let Some(html) = fetch_html_head(client, page.clone()).await {
        candidates.extend(declared_icons(&html, &page));
    }
    if let Ok(default_ico) = page.join("/favicon.ico") {
        candidates.push(default_ico);
    }

    for url in dedup(candidates).into_iter().take(MAX_CANDIDATES) {
        if let Some(uri) = try_icon(client, url).await {
            return Some(uri);
        }
    }
    None
}

/// Fetch (the start of) a page's HTML so we can scan its <head> for declared
/// icons. Truncation is fine here — icon links live near the top.
async fn fetch_html_head(client: &reqwest::Client, url: reqwest::Url) -> Option<String> {
    let (bytes, _) = fetch_capped(client, url, MAX_HTML_BYTES, true).await?;
    // Lossy is fine: we only scan for ASCII tags and attributes.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Fetch one candidate icon and, if it really is an image, return it as a
/// `data:` URI. Oversized or non-image responses are skipped (return `None`).
async fn try_icon(client: &reqwest::Client, url: reqwest::Url) -> Option<String> {
    let (bytes, content_type) = fetch_capped(client, url, MAX_ICON_BYTES, false).await?;
    if bytes.is_empty() {
        return None;
    }
    let mime = image_mime(content_type.as_deref(), &bytes)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

/// GET `url` with a timeout, returning the body (and its lowercased content-type
/// sans parameters). The body is capped at `cap` bytes: an HTML scan is happy
/// with a truncated head (`allow_truncate`), but a truncated icon would be a
/// broken image, so an oversized icon is rejected (returns `None`). Requests to
/// non-public hosts are refused outright.
async fn fetch_capped(
    client: &reqwest::Client,
    url: reqwest::Url,
    cap: usize,
    allow_truncate: bool,
) -> Option<(Vec<u8>, Option<String>)> {
    if host_is_blocked_url(&url) {
        return None;
    }
    let req = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes=0-{}", cap - 1));
    let resp = tokio::time::timeout(READ_TIMEOUT, req.send())
        .await
        .ok()?
        .ok()?;
    // 2xx (incl. 206 Partial Content if the server honored Range).
    if !resp.status().is_success() {
        return None;
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_ascii_lowercase());

    // Read incrementally so a lying Content-Length or ignored Range can't
    // stream unbounded data into memory.
    let mut bytes: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Ok(Some(chunk)) = tokio::time::timeout(READ_TIMEOUT, stream.next()).await {
        let chunk = chunk.ok()?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() > cap {
            if allow_truncate {
                bytes.truncate(cap);
                return Some((bytes, content_type));
            }
            return None;
        }
    }
    Some((bytes, content_type))
}

/// Icon URLs declared in the page's <head> (`<link rel="…icon…" href="…">`),
/// resolved to absolute http(s) URLs in document order (tried best-first).
/// Cross-origin hrefs are kept — a page may host its icon on a CDN, exactly as
/// a browser would load it — and the public-host gate at fetch time keeps any
/// of them from reaching an internal address.
fn declared_icons(html: &str, base: &reqwest::Url) -> Vec<reqwest::Url> {
    static LINK_TAG: OnceLock<Regex> = OnceLock::new();
    static REL_ATTR: OnceLock<Regex> = OnceLock::new();
    static HREF_ATTR: OnceLock<Regex> = OnceLock::new();
    let link_tag = LINK_TAG.get_or_init(|| Regex::new(r"(?is)<link\b[^>]*>").unwrap());
    let rel_attr = REL_ATTR.get_or_init(|| {
        Regex::new(r#"(?is)\brel\s*=\s*("([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap()
    });
    let href_attr = HREF_ATTR.get_or_init(|| {
        Regex::new(r#"(?is)\bhref\s*=\s*("([^"]*)"|'([^']*)'|([^\s"'>]+))"#).unwrap()
    });

    // Only the <head>: icon links live there, and it bounds the scan.
    let scan = match html.to_ascii_lowercase().find("</head>") {
        Some(end) => &html[..end],
        None => html,
    };

    let mut out = Vec::new();
    for tag in link_tag.find_iter(scan) {
        let tag = tag.as_str();
        let Some(rel) = rel_attr.captures(tag).and_then(|c| attr_value(&c)) else {
            continue;
        };
        if !rel.to_ascii_lowercase().contains("icon") {
            continue;
        }
        let Some(href) = href_attr.captures(tag).and_then(|c| attr_value(&c)) else {
            continue;
        };
        let href = href.trim().replace("&amp;", "&");
        if href.is_empty() {
            continue;
        }
        if let Ok(abs) = base.join(&href) {
            if matches!(abs.scheme(), "http" | "https") {
                out.push(abs);
            }
        }
    }
    out
}

/// The value of an attribute matched by one of the `*_ATTR` regexes, taking
/// whichever of the double-quoted / single-quoted / unquoted groups matched.
fn attr_value(c: &regex::Captures) -> Option<String> {
    c.get(2)
        .or_else(|| c.get(3))
        .or_else(|| c.get(4))
        .map(|m| m.as_str().to_string())
}

/// Best-effort MIME for an icon body, or `None` when the bytes clearly aren't an
/// image we should hand back. Magic bytes are trusted first — many servers send
/// `.ico`/`.svg` with a generic or wrong Content-Type — then a claimed `image/*`.
fn image_mime(content_type: Option<&str>, bytes: &[u8]) -> Option<&'static str> {
    if let Some(m) = sniff(bytes) {
        return Some(m);
    }
    match content_type {
        Some("image/png") => Some("image/png"),
        Some("image/jpeg") | Some("image/jpg") => Some("image/jpeg"),
        Some("image/gif") => Some("image/gif"),
        Some("image/webp") => Some("image/webp"),
        Some("image/svg+xml") => Some("image/svg+xml"),
        Some("image/x-icon") | Some("image/vnd.microsoft.icon") => Some("image/x-icon"),
        _ => None,
    }
}

/// Identify an image by its magic bytes — the reliable signal when servers
/// mislabel icons.
fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if b.starts_with(b"\xFF\xD8\xFF") {
        return Some("image/jpeg");
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    // ICO and CUR share the layout; both render fine as x-icon.
    if b.starts_with(b"\x00\x00\x01\x00") || b.starts_with(b"\x00\x00\x02\x00") {
        return Some("image/x-icon");
    }
    if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if looks_like_svg(b) {
        return Some("image/svg+xml");
    }
    None
}

/// SVG (possibly XML-prologued and indented): look for a `<svg` near the start.
fn looks_like_svg(b: &[u8]) -> bool {
    let head = &b[..b.len().min(512)];
    head.windows(4).any(|w| w.eq_ignore_ascii_case(b"<svg"))
}

fn dedup(urls: Vec<reqwest::Url>) -> Vec<reqwest::Url> {
    let mut seen = std::collections::HashSet::new();
    urls.into_iter()
        .filter(|u| seen.insert(u.as_str().to_string()))
        .collect()
}

fn host_is_blocked_url(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some(h) => host_is_blocked(h),
        None => true,
    }
}

/// True for hosts a favicon fetch must never touch: `localhost` and any literal
/// private / loopback / link-local / unspecified / CGNAT IP. Public domains and
/// IPs are allowed (the page chose where its icon lives). Note the URL crate
/// already normalizes encoded IPv4 forms (`2130706433`, `0x7f000001`, `0`) to
/// dotted-decimal, so those are caught here too. We block by literal host, so a
/// domain that *resolves* to an internal IP isn't caught — an accepted limit
/// for a best-effort, no-exfil icon fetch.
fn host_is_blocked(host: &str) -> bool {
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip_blocked(&ip),
        Err(_) => false, // a normal domain name — allowed
    }
}

fn ip_blocked(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 0 // 0.0.0.0/8 "this network"
                // 100.64.0.0/10 carrier-grade NAT (Ipv4Addr::is_shared is unstable)
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || v6
                    .to_ipv4_mapped()
                    .map(|m| ip_blocked(&IpAddr::V4(m)))
                    .unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn declared_icons_handles_attr_orders_and_relative_hrefs() {
        let base = url("https://example.com/some/page?q=1");
        let html = r#"
            <head>
              <link rel="stylesheet" href="/app.css">
              <link rel="icon" href="/favicon-32.png">
              <link href="https://cdn.example.com/apple.png" rel="apple-touch-icon">
              <link rel='shortcut icon' href='../icons/legacy.ico'>
              <link rel="mask-icon" href="/safari.svg">
            </head>
            <body><link rel="icon" href="/ignored-in-body.png"></body>
        "#;
        let got: Vec<String> = declared_icons(html, &base)
            .iter()
            .map(|u| u.as_str().to_string())
            .collect();
        assert_eq!(
            got,
            vec![
                "https://example.com/favicon-32.png".to_string(),
                // Cross-origin CDN is kept (browser-equivalent); the public-host
                // gate at fetch time still protects against internal targets.
                "https://cdn.example.com/apple.png".to_string(),
                // `../` from /some/page resolves against /some/ → /icons/… (RFC 3986).
                "https://example.com/icons/legacy.ico".to_string(),
                "https://example.com/safari.svg".to_string(),
            ]
        );
    }

    #[test]
    fn non_icon_rels_are_ignored() {
        let base = url("https://example.com/");
        let html = r#"<link rel="preload" href="/a.png"><link rel="canonical" href="/b">"#;
        assert!(declared_icons(html, &base).is_empty());
    }

    #[test]
    fn sniff_recognizes_common_formats() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....."), Some("image/png"));
        assert_eq!(sniff(b"\x00\x00\x01\x00rest"), Some("image/x-icon"));
        assert_eq!(sniff(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(
            sniff(b"   <?xml version='1.0'?><svg></svg>"),
            Some("image/svg+xml")
        );
        assert_eq!(sniff(b"not an image"), None);
    }

    #[test]
    fn image_mime_falls_back_to_content_type() {
        // A bare .ico body without a magic match still rides its header.
        assert_eq!(
            image_mime(Some("image/x-icon"), b"garbage"),
            Some("image/x-icon")
        );
        assert_eq!(
            image_mime(Some("image/png"), b"not-magic"),
            Some("image/png")
        );
        assert_eq!(image_mime(Some("text/html"), b"<html>"), None);
        // Magic wins over a mislabeled content-type.
        assert_eq!(
            image_mime(Some("application/octet-stream"), b"\x89PNG\r\n\x1a\n"),
            Some("image/png")
        );
    }

    #[test]
    fn blocks_internal_hosts_including_encoded_ipv4() {
        let blocked = |s: &str| host_is_blocked_url(&url(s));
        assert!(blocked("http://127.0.0.1/favicon.ico"));
        assert!(blocked("http://localhost:8080/"));
        assert!(blocked("http://api.localhost/"));
        assert!(blocked("http://10.0.0.5/"));
        assert!(blocked("http://192.168.1.1/"));
        assert!(blocked("http://172.16.0.1/"));
        assert!(blocked("http://169.254.169.254/latest/meta-data/"));
        assert!(blocked("http://0.0.0.0/"));
        assert!(blocked("http://100.64.0.1/"));
        assert!(blocked("http://[::1]/"));
        assert!(blocked("http://[fe80::1]/"));
        assert!(blocked("http://[fc00::1]/"));
        assert!(blocked("http://[::ffff:127.0.0.1]/"));
        // WHATWG URL parsing normalizes encoded IPv4 → dotted, so these are caught.
        assert!(blocked("http://2130706433/")); // 127.0.0.1
        assert!(blocked("http://0x7f000001/")); // 127.0.0.1
        assert!(blocked("http://0/")); // 0.0.0.0
    }

    #[test]
    fn allows_public_hosts() {
        let blocked = |s: &str| host_is_blocked_url(&url(s));
        assert!(!blocked("https://example.com/"));
        assert!(!blocked("https://cdn.example.com/icon.png"));
        assert!(!blocked("https://8.8.8.8/"));
        assert!(!blocked("https://[2606:2800:220:1:248:1893:25c8:1946]/"));
    }
}
