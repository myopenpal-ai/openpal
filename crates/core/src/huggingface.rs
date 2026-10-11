//! Talk to Hugging Face so users can add any GGUF model from there.
//!
//! Two pieces:
//! - [`parse_input`] understands what a user is likely to paste: a bare
//!   "owner/repo" id, a repo page URL, or a direct link to one file
//!   (`/resolve/<rev>/<file>` or `/blob/<rev>/<file>`, with or without
//!   `?download=true`).
//! - [`list_gguf_files`] asks the public tree API
//!   (`GET https://huggingface.co/api/models/<repo>/tree/<rev>?recursive=true`)
//!   which files exist. Each entry is `{type, path, size, lfs?}`; for LFS
//!   files `lfs.oid` is the file's SHA-256 and `lfs.size` its real byte size
//!   (verified HF API behavior). Vision adapters (`mmproj-*`) are filtered
//!   out — they aren't chat models. Split GGUFs (`...-00001-of-00003.gguf`)
//!   are grouped into one entry per complete shard set (llama-server opens
//!   shard 2..N automatically once given shard 1's path); an incomplete set
//!   is dropped rather than guessed at.
//!
//! All network errors become sentences a non-technical user can act on —
//! including the special case of gated/private repos (HTTP 401/403), which
//! need a Hugging Face login we don't have.

use crate::types::{
    HfFile, HfGgufInfo, HfListing, HfModelDetails, HfModelSummary, HfSearchPage, HfSearchQuery,
    HfShardFile, HfSortKey,
};
use crate::{Error, Result};
use futures_util::StreamExt;

const OFFLINE_MSG: &str =
    "Couldn't reach Hugging Face. Check your internet connection and try again.";
const UNPARSEABLE_MSG: &str =
    "Hugging Face sent back something unexpected. Please try again in a few minutes.";
const SERVER_ERROR_MSG: &str =
    "Hugging Face is having trouble right now. Please try again in a few minutes.";
const GATED_MSG: &str = "This model is gated on Hugging Face. It needs a sign-in and a license \
                          agreement on their website, so OpenPal can't download it for you.";

/// What the user pasted, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRef {
    /// "owner/repo".
    pub repo_id: String,
    /// Git revision (defaults to "main").
    pub revision: String,
    /// Set when the input pointed at one specific file in the repo.
    pub file_path: Option<String>,
}

const BAD_INPUT_MSG: &str = "That doesn't look like a Hugging Face model. Paste a link like \
                             huggingface.co/TheOrg/TheModel-GGUF, or just \"TheOrg/TheModel-GGUF\".";

/// Normalize whatever the user pasted into an [`HfRef`].
pub fn parse_input(input: &str) -> Result<HfRef> {
    let trimmed = input.trim();
    // Strip a query string / fragment ("?download=true" on copied links).
    let trimmed = trimmed.split(['?', '#']).next().unwrap_or("");

    // Strip URL prefixes down to the path inside huggingface.co.
    let path = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let path = path
        .strip_prefix("huggingface.co/")
        .or_else(|| path.strip_prefix("www.huggingface.co/"))
        .or_else(|| path.strip_prefix("hf.co/"))
        .unwrap_or(path);
    let path = path.trim_matches('/');

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 || !valid_segment(segments[0], false) || !valid_segment(segments[1], true)
    {
        return Err(Error::Download(BAD_INPUT_MSG.to_string()));
    }
    let repo_id = format!("{}/{}", segments[0], segments[1]);

    // owner/repo[/(resolve|blob)/<rev>/<file...>]
    match segments.get(2) {
        None => Ok(HfRef {
            repo_id,
            revision: "main".to_string(),
            file_path: None,
        }),
        Some(&"resolve") | Some(&"blob") if segments.len() >= 5 => Ok(HfRef {
            repo_id,
            revision: segments[3].to_string(),
            file_path: Some(segments[4..].join("/")),
        }),
        // Other repo sub-pages ("/tree/main", "/discussions", …): treat as
        // the repo itself.
        Some(_) => Ok(HfRef {
            repo_id,
            revision: "main".to_string(),
            file_path: None,
        }),
    }
}

/// Owners are alphanumeric with dashes/underscores; repo names may also
/// contain dots. Rejecting dots in the owner conveniently rejects pastes from
/// other websites ("example.com/a/b"). A segment that is exactly "." or ".."
/// is always rejected — allowed elsewhere in the charset, but as a whole
/// segment it's a path-traversal attempt once interpolated into a URL.
/// Shared by `parse_input` (pasted links) and `validate_repo_id`
/// (browser-supplied repo ids) so both accept exactly the same charset.
fn valid_segment(s: &str, allow_dots: bool) -> bool {
    if s.is_empty() || s == "." || s == ".." {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || (allow_dots && c == '.'))
}

/// Reject a repo id that isn't safe to interpolate into a Hugging Face URL.
/// Unlike `parse_input`, this never sees a full pasted link — just the
/// "owner/repo" the browse UI already resolved — so it only needs to check
/// the charset `parse_input` already enforces for pasted links.
fn validate_repo_id(repo_id: &str) -> Result<()> {
    match repo_id.split_once('/') {
        Some((owner, repo)) if valid_segment(owner, false) && valid_segment(repo, true) => Ok(()),
        _ => Err(Error::Download(BAD_INPUT_MSG.to_string())),
    }
}

/// Reject a revision that isn't safe to interpolate into a Hugging Face URL.
/// Revisions may contain slashes (e.g. "refs/pr/12"), so this can't reuse
/// `valid_segment`; it instead charset-restricts and blanket-rejects any
/// ".." to close the same path-traversal risk.
fn validate_revision(revision: &str) -> Result<()> {
    let charset_ok = !revision.is_empty()
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    let safe = charset_ok
        && !revision.contains("..")
        && !revision.starts_with('/')
        && !revision.ends_with('/');
    if safe {
        Ok(())
    } else {
        Err(Error::Download(
            "That revision doesn't look valid.".to_string(),
        ))
    }
}

/// Direct download URL for one file of a repo.
pub fn resolve_url(repo_id: &str, revision: &str, file_path: &str) -> String {
    format!("https://huggingface.co/{repo_id}/resolve/{revision}/{file_path}")
}

/// Range-fetch sizes to try, in order, when the GGUF metadata section
/// doesn't fit in the first attempt — a large tokenizer vocab array can push
/// the key-value section (which precedes tensor-info, see
/// [`crate::gguf_meta`]) well past a small prefix. Capped rather than
/// growing forever: a file that still doesn't parse at 64 MiB just fails
/// open, same as any other lookup here.
const GGUF_META_FETCH_SIZES: [u64; 3] = [4 * 1024 * 1024, 16 * 1024 * 1024, 64 * 1024 * 1024];

const GGUF_META_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Best-effort real MoE/parameter-count detection for one Hugging Face GGUF
/// file: range-fetches a prefix of the file and parses its own binary header
/// ([`crate::gguf_meta`]) — the same source of truth `custom::local_spec`
/// reads directly off disk for local files. Retries with a larger range only
/// when the parse reports [`crate::gguf_meta::GgufMetaError::Truncated`];
/// any other failure (bad input, offline, an unparseable file) is returned
/// immediately. Callers always have a size/quant-based estimate to fall back
/// to, so this never needs to block anything — see `commands::cached_gguf_meta`.
pub async fn fetch_gguf_header_meta(
    client: &reqwest::Client,
    repo_id: &str,
    revision: &str,
    file_path: &str,
) -> Result<crate::gguf_meta::GgufMeta> {
    validate_repo_id(repo_id)?;
    validate_revision(revision)?;
    let url = resolve_url(repo_id, revision, file_path);

    for &cap in &GGUF_META_FETCH_SIZES {
        let bytes = fetch_range_capped(client, &url, cap).await?;
        match crate::gguf_meta::parse_bytes(&bytes) {
            Ok(meta) => return Ok(meta),
            Err(crate::gguf_meta::GgufMetaError::Truncated) => continue,
            Err(crate::gguf_meta::GgufMetaError::Malformed(msg)) => {
                return Err(Error::Download(format!(
                    "Couldn't read this file's own metadata ({msg})"
                )));
            }
        }
    }
    Err(Error::Download(UNPARSEABLE_MSG.to_string()))
}

/// GET a byte-range prefix of `url`, reading incrementally so a server that
/// ignores `Range` (or lies about it) can't stream an entire multi-gigabyte
/// model file into memory — same capped-read shape as `favicon::fetch_capped`.
async fn fetch_range_capped(client: &reqwest::Client, url: &str, cap: u64) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes=0-{}", cap - 1))
        .send()
        .await
        .map_err(|_| Error::Download(OFFLINE_MSG.to_string()))?;
    // 2xx (incl. 206 Partial Content if the server honored Range).
    if !resp.status().is_success() {
        return Err(Error::Download(UNPARSEABLE_MSG.to_string()));
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = tokio::time::timeout(GGUF_META_FETCH_TIMEOUT, stream.next())
        .await
        .map_err(|_| Error::Download(OFFLINE_MSG.to_string()))?
    {
        let chunk = chunk.map_err(|_| Error::Download(OFFLINE_MSG.to_string()))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= cap {
            bytes.truncate(cap as usize);
            break;
        }
    }
    Ok(bytes)
}

/// List the single-file GGUFs `hf_ref` offers, smallest first.
pub async fn list_gguf_files(client: &reqwest::Client, hf_ref: &HfRef) -> Result<HfListing> {
    let url = format!(
        "https://huggingface.co/api/models/{}/tree/{}?recursive=true",
        hf_ref.repo_id, hf_ref.revision
    );
    let response = client.get(&url).send().await.map_err(|e| {
        tracing::warn!("hf tree request failed: {e}");
        Error::Download(OFFLINE_MSG.to_string())
    })?;

    match response.status() {
        status if status.is_success() => {}
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            return Err(Error::Download(GATED_MSG.to_string()));
        }
        reqwest::StatusCode::NOT_FOUND => {
            return Err(Error::Download(format!(
                "Couldn't find \"{}\" on Hugging Face. Double-check the link or name.",
                hf_ref.repo_id
            )));
        }
        status => {
            tracing::warn!("hf tree request rejected: {status} for {url}");
            return Err(Error::Download(format!(
                "Hugging Face couldn't list that model (it replied with code {}). Please try \
                 again in a few minutes.",
                status.as_u16()
            )));
        }
    }

    #[derive(serde::Deserialize)]
    struct TreeEntry {
        #[serde(rename = "type")]
        kind: String,
        path: String,
        #[serde(default)]
        size: u64,
        #[serde(default)]
        lfs: Option<TreeLfs>,
    }
    #[derive(serde::Deserialize)]
    struct TreeLfs {
        oid: String,
        size: u64,
    }

    let entries: Vec<TreeEntry> = response.json().await.map_err(|e| {
        tracing::warn!("hf tree response unparseable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;

    let raw: Vec<RawGguf> = entries
        .into_iter()
        .filter(|e| e.kind == "file" && e.path.to_ascii_lowercase().ends_with(".gguf"))
        .filter(|e| {
            // Vision adapters accompany a main model; they aren't chat models.
            !file_name(&e.path)
                .to_ascii_lowercase()
                .starts_with("mmproj")
        })
        .map(|e| RawGguf {
            path: e.path,
            size_bytes: e
                .lfs
                .as_ref()
                .map(|l| l.size)
                .filter(|&s| s > 0)
                .unwrap_or(e.size),
            sha256: e.lfs.map(|l| l.oid),
        })
        .collect();

    let (mut files, saw_split_files) = group_gguf_files(raw);
    files.sort_by_key(HfFile::total_size_bytes);

    if files.is_empty() {
        let message = if saw_split_files {
            "This model's files are split into multiple parts, but OpenPal couldn't find a \
             complete set. Some parts may be missing or gated. Look for a version with a full \
             set of parts (or single-file downloads)."
        } else {
            "That Hugging Face page has no GGUF files. OpenPal needs a GGUF version. Try \
             searching Hugging Face for the model's name plus \"GGUF\"."
        };
        return Err(Error::Download(message.to_string()));
    }

    let preselect_path = match &hf_ref.file_path {
        Some(wanted)
            if files
                .iter()
                .any(|f| &f.path == wanted || f.extra_shards.iter().any(|s| &s.path == wanted)) =>
        {
            Some(wanted.clone())
        }
        _ => None,
    };

    Ok(HfListing {
        repo_id: hf_ref.repo_id.clone(),
        revision: hf_ref.revision.clone(),
        files,
        preselect_path,
    })
}

/// "subdir/model-Q4_K_M.gguf" -> "model-Q4_K_M.gguf"
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One GGUF file from the repo tree, before grouping split shards together.
struct RawGguf {
    path: String,
    size_bytes: u64,
    sha256: Option<String>,
}

/// Parse a GGUF file name's split-shard suffix ("...-00001-of-00003.gguf"),
/// returning `(stem, index, total)` where `stem` is everything before the
/// suffix — identical across every shard of one model, since llama.cpp
/// derives shard 2..N's expected file names from shard 1's by substituting
/// just the index. `None` when the name isn't a split-GGUF shard, or the
/// index/total don't form a sane 1-based pair.
pub(crate) fn split_shard_info(lower_name: &str) -> Option<(&str, u32, u32)> {
    let stem = lower_name.strip_suffix(".gguf")?;
    let (rest, total) = stem.rsplit_once('-')?;
    let total: u32 = (total.len() == 5).then(|| total.parse().ok()).flatten()?;
    let (rest, of) = rest.rsplit_once('-')?;
    if of != "of" {
        return None;
    }
    let (stem, index) = rest.rsplit_once('-')?;
    let index: u32 = (index.len() == 5).then(|| index.parse().ok()).flatten()?;
    (index >= 1 && index <= total).then_some((stem, index, total))
}

/// Group raw GGUF file candidates into the `HfFile`s a repo listing shows:
/// ordinary files pass through unchanged; split-GGUF shards are grouped by
/// (parent directory, stem, total) and kept only when a complete, contiguous
/// `1..=total` set is present — an incomplete group (gated repo, odd layout)
/// can't actually be loaded by llama-server, so it's dropped rather than
/// guessed at. Pure and HTTP-free so it's directly unit-testable. Returns the
/// grouped files (unsorted) plus whether any split-shard file was seen at all
/// (only used to word the "nothing here" error when the result is empty).
fn group_gguf_files(raw: Vec<RawGguf>) -> (Vec<HfFile>, bool) {
    use std::collections::HashMap;

    let mut singles = Vec::new();
    let mut groups: HashMap<(String, String, u32), Vec<(u32, RawGguf)>> = HashMap::new();
    let mut saw_split = false;

    for f in raw {
        let name = file_name(&f.path).to_ascii_lowercase();
        match split_shard_info(&name) {
            Some((stem, index, total)) => {
                saw_split = true;
                let dir = f
                    .path
                    .rsplit_once('/')
                    .map(|(d, _)| d.to_string())
                    .unwrap_or_default();
                groups
                    .entry((dir, stem.to_string(), total))
                    .or_default()
                    .push((index, f));
            }
            None => singles.push(HfFile {
                quant: quant_from_file_name(file_name(&f.path)),
                size_bytes: f.size_bytes,
                sha256: f.sha256,
                path: f.path,
                extra_shards: Vec::new(),
            }),
        }
    }

    let mut files = singles;
    for ((_, _, total), mut parts) in groups {
        parts.sort_by_key(|(index, _)| *index);
        let complete = parts.len() as u32 == total
            && parts
                .iter()
                .enumerate()
                .all(|(i, (index, _))| *index == i as u32 + 1);
        if !complete {
            continue; // partial/gated shard set — can't be loaded, don't guess
        }
        let mut shards = parts.into_iter().map(|(_, f)| f);
        let first = shards
            .next()
            .expect("a complete group has at least shard 1");
        let extra_shards = shards
            .map(|f| HfShardFile {
                path: f.path,
                size_bytes: f.size_bytes,
                sha256: f.sha256,
            })
            .collect();
        files.push(HfFile {
            quant: quant_from_file_name(file_name(&first.path)),
            size_bytes: first.size_bytes,
            sha256: first.sha256,
            path: first.path,
            extra_shards,
        });
    }
    (files, saw_split)
}

/// Best-effort quantization label from a GGUF file name ("Q4_K_M", "IQ4_XS",
/// "F16", …). Falls back to "GGUF" when nothing recognizable is found.
pub fn quant_from_file_name(name: &str) -> String {
    let stem = name
        .strip_suffix(".gguf")
        .or_else(|| name.strip_suffix(".GGUF"))
        .unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    // A quant label starts at a separator ("-", ".", "_", " ") and runs to
    // the next non-underscore separator: "…-Q4_K_M", "….Q5_K_M", "…_Q8_0",
    // "…-IQ2_XXS". The right-most match wins.
    let bytes = upper.as_bytes();
    let mut best: Option<&str> = None;
    for i in 0..upper.len() {
        if !upper.is_char_boundary(i)
            || !(i == 0 || matches!(bytes[i - 1], b'-' | b'.' | b'_' | b' '))
        {
            continue;
        }
        let rest = &upper[i..];
        let token = &rest[..rest.find(['-', '.', ' ']).unwrap_or(rest.len())];
        let token = token.trim_matches('_');
        let known = token
            .strip_prefix("IQ")
            .or_else(|| token.strip_prefix('Q'))
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_digit());
        if known || matches!(token, "F16" | "F32" | "BF16" | "FP16" | "FP32") {
            best = Some(token);
        }
    }
    best.map(str::to_string)
        .unwrap_or_else(|| "GGUF".to_string())
}

/// Bits-per-weight by quant family; unknown quants assume ~4.5 bpw (the
/// common Q4 case). Shared by `estimate_params_b` and its inverse,
/// `estimate_size_bytes`.
fn bits_per_weight(quant: &str) -> f64 {
    let upper = quant.to_ascii_uppercase();
    let digit = upper
        .trim_start_matches("IQ")
        .trim_start_matches('Q')
        .chars()
        .next()
        .and_then(|c| c.to_digit(10));
    match (upper.as_str(), digit) {
        ("F32" | "FP32", _) => 32.0,
        ("F16" | "FP16" | "BF16", _) => 16.0,
        (_, Some(1)) => 1.8,
        (_, Some(2)) => 2.8,
        (_, Some(3)) => 3.6,
        (_, Some(4)) => 4.7,
        (_, Some(5)) => 5.6,
        (_, Some(6)) => 6.6,
        (_, Some(8)) => 8.5,
        _ => 4.5,
    }
}

/// Rough parameter count (in billions) from file size and quant label, for
/// the memory-fit estimate.
pub fn estimate_params_b(size_bytes: u64, quant: &str) -> f32 {
    (size_bytes as f64 * 8.0 / bits_per_weight(quant) / 1e9) as f32
}

/// Inverse of `estimate_params_b`: the file size a model with `params_b`
/// billion parameters would have at `quant`. Used to build a synthetic file
/// size when only a repo's parameter count is known (the hub's search
/// listing, before the user has picked an actual quant) — see
/// `HfModelSummary.fit` and docs/design/HF_BROWSER.md decision 6.
pub fn estimate_size_bytes(params_b: f32, quant: &str) -> u64 {
    (params_b as f64 * 1e9 * bits_per_weight(quant) / 8.0) as u64
}

// ---------------------------------------------------------------------------
// Hub browser: search / trending, model card, README
// ---------------------------------------------------------------------------

impl HfSortKey {
    fn api_value(self) -> &'static str {
        match self {
            HfSortKey::Trending => "trendingScore",
            HfSortKey::Downloads => "downloads",
            HfSortKey::Likes => "likes",
            HfSortKey::LastModified => "lastModified",
        }
    }
}

/// Fields requested via `expand[]` for both the search listing and (where
/// applicable) reused verbatim: once any `expand[]` is present, the API
/// returns *only* the expanded fields plus `id`/`trendingScore`, so every
/// field a result card needs must be listed here.
const SEARCH_EXPAND_FIELDS: &[&str] = &[
    "author",
    "downloads",
    "likes",
    "tags",
    "gated",
    "lastModified",
    "pipeline_tag",
    "library_name",
    "gguf",
];

const SEARCH_PAGE_LIMIT: &str = "30";

fn build_search_url(query: &HfSearchQuery) -> reqwest::Url {
    let mut url =
        reqwest::Url::parse("https://huggingface.co/api/models").expect("static URL is valid");
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("filter", "gguf");
        qp.append_pair("direction", "-1");
        qp.append_pair("limit", SEARCH_PAGE_LIMIT);
        qp.append_pair("sort", query.sort.api_value());
        for field in SEARCH_EXPAND_FIELDS {
            qp.append_pair("expand[]", field);
        }
        if let Some(s) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            qp.append_pair("search", s);
        }
        if let Some(a) = query
            .author
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            qp.append_pair("author", a);
        }
        if let Some(p) = query
            .pipeline_tag
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            qp.append_pair("pipeline_tag", p);
        }
        if let Some(c) = query.cursor.as_deref().filter(|c| !c.is_empty()) {
            qp.append_pair("cursor", c);
        }
    }
    url
}

/// Search (or, with an empty query, browse trending) GGUF repos on the hub.
pub async fn search_models(
    client: &reqwest::Client,
    query: &HfSearchQuery,
) -> Result<HfSearchPage> {
    let url = build_search_url(query);
    let response = client.get(url).send().await.map_err(|e| {
        tracing::warn!("hf search request failed: {e}");
        Error::Download(OFFLINE_MSG.to_string())
    })?;
    if let Some(err) =
        hf_response_error(&response, "Hugging Face couldn't find any matching models.")
    {
        return Err(err);
    }
    let next_cursor = next_cursor_from_link_header(response.headers());
    let body = response.text().await.map_err(|e| {
        tracing::warn!("hf search response unreadable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;
    parse_search_response(&body, next_cursor)
}

/// The hub's `gguf` object, shared by the search-listing and model-detail
/// responses (`expand[]=gguf` on the former, present by default on the
/// latter) — kept at module scope so both parsers agree on its shape.
#[derive(serde::Deserialize)]
struct RawGgufMeta {
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
}

impl From<RawGgufMeta> for HfGgufInfo {
    fn from(raw: RawGgufMeta) -> Self {
        HfGgufInfo {
            total_params: raw.total,
            architecture: raw.architecture,
            context_length: raw.context_length,
        }
    }
}

fn parse_search_response(body: &str, next_cursor: Option<String>) -> Result<HfSearchPage> {
    #[derive(serde::Deserialize)]
    struct RawItem {
        id: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        pipeline_tag: Option<String>,
        #[serde(default)]
        library_name: Option<String>,
        #[serde(default)]
        downloads: Option<u64>,
        #[serde(default)]
        likes: Option<u64>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        gated: serde_json::Value,
        // The hub API returns this key in camelCase, unlike most of this
        // struct's other fields (`pipeline_tag`, `library_name`) — without
        // the explicit rename this silently never deserializes against the
        // real API and `last_modified` stays `None` forever.
        #[serde(default, rename = "lastModified")]
        last_modified: Option<String>,
        #[serde(default)]
        gguf: Option<RawGgufMeta>,
    }

    let items: Vec<RawItem> = serde_json::from_str(body).map_err(|e| {
        tracing::warn!("hf search response unparseable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;

    let models = items
        .into_iter()
        .map(|it| HfModelSummary {
            repo_id: it.id,
            author: it.author,
            pipeline_tag: it.pipeline_tag,
            library_name: it.library_name,
            downloads: it.downloads,
            likes: it.likes,
            license: license_from_tags(&it.tags),
            gated: gated_from_value(&it.gated),
            last_modified: it.last_modified,
            gguf: it.gguf.map(Into::into),
            // Needs `HardwareInfo`; filled in by the command layer.
            fit: None,
            fit_why: None,
        })
        .collect();

    Ok(HfSearchPage {
        models,
        next_cursor,
    })
}

/// Fetch one repo's model-card metadata (downloads, likes, license, GGUF
/// architecture/context length when the hub could parse it).
pub async fn model_details(client: &reqwest::Client, repo_id: &str) -> Result<HfModelDetails> {
    validate_repo_id(repo_id)?;
    let url = format!("https://huggingface.co/api/models/{repo_id}");
    let response = client.get(&url).send().await.map_err(|e| {
        tracing::warn!("hf model-details request failed: {e}");
        Error::Download(OFFLINE_MSG.to_string())
    })?;
    let not_found =
        format!("Couldn't find \"{repo_id}\" on Hugging Face. Double-check the link or name.");
    if let Some(err) = hf_response_error(&response, &not_found) {
        return Err(err);
    }
    let body = response.text().await.map_err(|e| {
        tracing::warn!("hf model-details response unreadable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;
    parse_model_details(&body)
}

fn parse_model_details(body: &str) -> Result<HfModelDetails> {
    #[derive(serde::Deserialize)]
    struct RawDetail {
        id: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        pipeline_tag: Option<String>,
        #[serde(default)]
        downloads: Option<u64>,
        #[serde(default)]
        likes: Option<u64>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        gated: serde_json::Value,
        // These two keys come back in camelCase from the real hub API,
        // unlike most of this struct's fields — without the explicit rename
        // they silently never deserialize and both fields stay `None`
        // forever (a latent bug: the canned test fixtures below happen to
        // exercise the `tags`-based license fallback instead, so it went
        // uncaught).
        #[serde(default, rename = "lastModified")]
        last_modified: Option<String>,
        #[serde(default, rename = "createdAt")]
        created_at: Option<String>,
        #[serde(default, rename = "cardData")]
        card_data: Option<RawCardData>,
        #[serde(default)]
        gguf: Option<RawGgufMeta>,
    }
    #[derive(serde::Deserialize)]
    struct RawCardData {
        #[serde(default)]
        license: Option<String>,
    }

    let raw: RawDetail = serde_json::from_str(body).map_err(|e| {
        tracing::warn!("hf model-details response unparseable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;

    let license = raw
        .card_data
        .and_then(|c| c.license)
        .or_else(|| license_from_tags(&raw.tags));
    Ok(HfModelDetails {
        repo_id: raw.id,
        author: raw.author,
        pipeline_tag: raw.pipeline_tag,
        downloads: raw.downloads,
        likes: raw.likes,
        license,
        gated: gated_from_value(&raw.gated),
        last_modified: raw.last_modified,
        created_at: raw.created_at,
        tags: raw.tags,
        gguf: raw.gguf.map(Into::into),
    })
}

/// Fetch a repo's README (model card body), with the leading YAML
/// frontmatter stripped — the same data is already available structured via
/// `HfModelDetails`, so the body is what's worth rendering. Capped at 1 MiB.
pub async fn fetch_readme(
    client: &reqwest::Client,
    repo_id: &str,
    revision: &str,
) -> Result<String> {
    validate_repo_id(repo_id)?;
    validate_revision(revision)?;
    let url = format!("https://huggingface.co/{repo_id}/raw/{revision}/README.md");
    let response = client.get(&url).send().await.map_err(|e| {
        tracing::warn!("hf readme request failed: {e}");
        Error::Download(OFFLINE_MSG.to_string())
    })?;
    if let Some(err) = hf_response_error(&response, "This model doesn't have a README yet.") {
        return Err(err);
    }
    // The URL's host is a hardcoded literal, not attacker-controlled, but the
    // response can still be a redirect (e.g. to a CDN mirror) — confirm the
    // final host is still Hugging Face's before trusting the body.
    if !response.url().host_str().is_some_and(is_hf_host) {
        return Err(Error::Download(
            "Hugging Face redirected somewhere unexpected, so the model card couldn't be loaded."
                .to_string(),
        ));
    }
    const MAX_README_BYTES: usize = 1_048_576;
    let bytes = response.bytes().await.map_err(|e| {
        tracing::warn!("hf readme response unreadable: {e}");
        Error::Download(UNPARSEABLE_MSG.to_string())
    })?;
    // Decode first, cap second: reqwest already buffered the whole body
    // before we get here, so this costs nothing extra, and it lets the cap
    // back off to a real character boundary instead of slicing raw bytes
    // (which can land mid-character and surface as a stray replacement
    // character at the cut point).
    let text = String::from_utf8_lossy(&bytes);
    let capped = truncate_at_char_boundary(&text, MAX_README_BYTES);
    Ok(strip_frontmatter(capped))
}

/// Truncate `s` to at most `max_bytes`, backing off to the nearest earlier
/// UTF-8 character boundary so a multi-byte character is never split.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn is_hf_host(host: &str) -> bool {
    host == "huggingface.co"
        || host.ends_with(".huggingface.co")
        || host == "hf.co"
        || host.ends_with(".hf.co")
}

/// Turn a non-2xx Hugging Face response into a friendly `Error::Download`;
/// `None` means the caller should proceed. Shared by `search_models`,
/// `model_details`, and `fetch_readme`. `not_found_message` lets each caller
/// phrase the 404 case for what it was looking up.
fn hf_response_error(response: &reqwest::Response, not_found_message: &str) -> Option<Error> {
    let status = response.status();
    if status.is_success() {
        return None;
    }
    Some(match status {
        reqwest::StatusCode::NOT_FOUND => Error::Download(not_found_message.to_string()),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Error::Download(GATED_MSG.to_string())
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            // `ratelimit` is what Hugging Face's API sends (verified live,
            // `"api";r=0;t=30`); `Retry-After` is the standard HTTP header
            // other endpoints (or a future HF change) might use instead —
            // try both rather than assuming one.
            let headers = response.headers();
            let wait = headers
                .get("ratelimit")
                .and_then(|v| v.to_str().ok())
                .and_then(parse_retry_seconds)
                .or_else(|| {
                    headers
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                })
                .unwrap_or(60);
            Error::Download(format!(
                "Hugging Face is asking us to slow down. Try again in about {wait} seconds."
            ))
        }
        status if status.is_server_error() => Error::Download(SERVER_ERROR_MSG.to_string()),
        status => Error::Download(format!(
            "Hugging Face couldn't complete that request (code {}). Please try again.",
            status.as_u16()
        )),
    })
}

/// Extract `t=<seconds>` from a `RateLimit` header value like
/// `"api";r=0;t=30`.
fn parse_retry_seconds(header_value: &str) -> Option<u64> {
    header_value
        .split(';')
        .find_map(|part| part.trim().strip_prefix("t=")?.parse().ok())
}

/// The hub's `gated` field is `false | "auto" | "manual"` — a bool/string
/// union. Both string variants mean "gated"; anything else (including absent
/// / null, when a caller didn't request the field) means "not gated".
fn gated_from_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => s != "false" && !s.is_empty(),
        _ => false,
    }
}

/// License chip from a list item's `tags` (list endpoints don't carry
/// `cardData`); e.g. `"license:apache-2.0"` -> `"apache-2.0"`.
fn license_from_tags(tags: &[String]) -> Option<String> {
    tags.iter()
        .find_map(|t| t.strip_prefix("license:").map(str::to_string))
}

/// Parse the `next` cursor out of a `Link: <url>; rel="next", <url2>;
/// rel="prev"` response header.
fn next_cursor_from_link_header(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let raw = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    for part in raw.split(',') {
        let mut segments = part.split(';').map(str::trim);
        let url_part = segments.next()?;
        let is_next = segments.any(|s| s == "rel=\"next\"" || s == "rel=next");
        if !is_next {
            continue;
        }
        let url_str = url_part.trim_start_matches('<').trim_end_matches('>');
        let url = reqwest::Url::parse(url_str).ok()?;
        return url
            .query_pairs()
            .find(|(k, _)| k == "cursor")
            .map(|(_, v)| v.into_owned());
    }
    None
}

/// Strip a leading `---`-delimited YAML frontmatter block from a model card.
/// Returns the input unchanged if there's no well-formed frontmatter block.
fn strip_frontmatter(markdown: &str) -> String {
    let mut lines = markdown.lines();
    if !matches!(lines.next(), Some(first) if first.trim() == "---") {
        return markdown.to_string();
    }
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
    }
    if !closed {
        return markdown.to_string();
    }
    lines.collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_repo_ids_and_repo_urls() {
        for input in [
            "unsloth/Qwen3-4B-GGUF",
            "  unsloth/Qwen3-4B-GGUF  ",
            "https://huggingface.co/unsloth/Qwen3-4B-GGUF",
            "http://hf.co/unsloth/Qwen3-4B-GGUF/",
            "huggingface.co/unsloth/Qwen3-4B-GGUF/tree/main",
        ] {
            let r = parse_input(input).unwrap();
            assert_eq!(r.repo_id, "unsloth/Qwen3-4B-GGUF", "{input}");
            assert_eq!(r.revision, "main");
            assert_eq!(r.file_path, None, "{input}");
        }
    }

    #[test]
    fn parses_direct_file_links() {
        let r = parse_input(
            "https://huggingface.co/unsloth/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf?download=true",
        )
        .unwrap();
        assert_eq!(r.repo_id, "unsloth/Qwen3-4B-GGUF");
        assert_eq!(r.revision, "main");
        assert_eq!(r.file_path.as_deref(), Some("Qwen3-4B-Q4_K_M.gguf"));

        // /blob/ page links and nested paths work too.
        let r = parse_input("hf.co/o/r/blob/v2/sub/dir/m.gguf").unwrap();
        assert_eq!(r.revision, "v2");
        assert_eq!(r.file_path.as_deref(), Some("sub/dir/m.gguf"));
    }

    #[test]
    fn rejects_things_that_are_not_models() {
        for input in [
            "",
            "qwen",
            "https://example.com/a/b",
            "a b/c d",
            "ow ner/repo",
        ] {
            assert!(parse_input(input).is_err(), "{input:?} should be rejected");
        }
    }

    #[test]
    fn split_shard_info_parses_stem_index_and_total() {
        assert_eq!(
            split_shard_info("gpt-oss-120b-mxfp4-00001-of-00003.gguf"),
            Some(("gpt-oss-120b-mxfp4", 1, 3))
        );
        assert_eq!(
            split_shard_info("gpt-oss-120b-mxfp4-00003-of-00003.gguf"),
            Some(("gpt-oss-120b-mxfp4", 3, 3))
        );
        // Index 0 and index > total aren't sane 1-based shard numbers.
        assert_eq!(split_shard_info("model-00000-of-00003.gguf"), None);
        assert_eq!(split_shard_info("model-00004-of-00003.gguf"), None);
        assert_eq!(split_shard_info("model-q4_k_m.gguf"), None);
    }

    fn raw(path: &str, size_bytes: u64) -> RawGguf {
        RawGguf {
            path: path.to_string(),
            size_bytes,
            sha256: Some(format!("sha-{path}")),
        }
    }

    #[test]
    fn group_gguf_files_groups_a_complete_shard_set_into_one_entry() {
        let (files, saw_split) = group_gguf_files(vec![
            raw("model-00002-of-00003.gguf", 20),
            raw("single.gguf", 5),
            raw("model-00001-of-00003.gguf", 10),
            raw("model-00003-of-00003.gguf", 30),
        ]);
        assert!(saw_split);
        assert_eq!(files.len(), 2, "one grouped entry + one single file");

        let group = files
            .iter()
            .find(|f| f.path.contains("00001"))
            .expect("group present");
        assert_eq!(group.size_bytes, 10, "shard 1's own size");
        assert_eq!(
            group.total_size_bytes(),
            60,
            "10 + 20 + 30 across all shards"
        );
        let shard_paths: Vec<&str> = group.extra_shards.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            shard_paths,
            ["model-00002-of-00003.gguf", "model-00003-of-00003.gguf"]
        );

        let single = files
            .iter()
            .find(|f| f.path == "single.gguf")
            .expect("single file kept");
        assert!(single.extra_shards.is_empty());
    }

    #[test]
    fn group_gguf_files_drops_an_incomplete_shard_set() {
        // Only 2 of 3 parts listed (e.g. a gated/partial repo) — can't be
        // loaded by llama-server, so it must not appear at all.
        let (files, saw_split) = group_gguf_files(vec![
            raw("model-00001-of-00003.gguf", 10),
            raw("model-00003-of-00003.gguf", 30),
        ]);
        assert!(files.is_empty());
        assert!(
            saw_split,
            "still flagged so the error message mentions split files"
        );
    }

    #[test]
    fn group_gguf_files_keeps_shard_groups_from_different_directories_separate() {
        let (files, _) = group_gguf_files(vec![
            raw("a/model-00001-of-00002.gguf", 10),
            raw("a/model-00002-of-00002.gguf", 10),
            raw("b/model-00001-of-00002.gguf", 99),
            raw("b/model-00002-of-00002.gguf", 99),
        ]);
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extra_shards.len() == 1));
    }

    #[test]
    fn quant_labels_come_from_file_names() {
        assert_eq!(
            quant_from_file_name("Llama-3.2-1B-Instruct-Q4_K_M.gguf"),
            "Q4_K_M"
        );
        assert_eq!(quant_from_file_name("mistral-7b.Q5_K_M.gguf"), "Q5_K_M");
        assert_eq!(quant_from_file_name("model-IQ2_XXS.gguf"), "IQ2_XXS");
        assert_eq!(quant_from_file_name("gemma-f16.gguf"), "F16");
        assert_eq!(quant_from_file_name("weird-name.gguf"), "GGUF");
    }

    /// Network test against the real Hugging Face API; run explicitly with
    /// `cargo test -p openpal-core huggingface -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn lists_a_real_repo() {
        let hf_ref = parse_input("lmstudio-community/SmolLM2-135M-Instruct-GGUF").unwrap();
        let listing = list_gguf_files(&reqwest::Client::new(), &hf_ref)
            .await
            .unwrap();
        assert_eq!(
            listing.repo_id,
            "lmstudio-community/SmolLM2-135M-Instruct-GGUF"
        );

        // The Q8_0 file is the one our embedded catalog pins; the live API
        // must agree with the verified size and sha256.
        let q8 = listing
            .files
            .iter()
            .find(|f| f.path == "SmolLM2-135M-Instruct-Q8_0.gguf")
            .expect("Q8_0 file listed");
        assert_eq!(q8.quant, "Q8_0");
        assert_eq!(q8.size_bytes, 144_811_072);
        assert_eq!(
            q8.sha256.as_deref(),
            Some("40f7094960b6ede829145d102ca79451b364b27d9d8694d4406e002024cff357")
        );
        // Sorted smallest-first.
        assert!(listing
            .files
            .windows(2)
            .all(|w| w[0].size_bytes <= w[1].size_bytes));
    }

    #[test]
    fn params_estimates_are_in_the_right_ballpark() {
        // Llama 3.2 1B Q4_K_M is ~0.8 GB for 1.24B params.
        let b = estimate_params_b(807_690_688, "Q4_K_M");
        assert!((1.0..2.0).contains(&b), "got {b}");
        // A 16 GB F16 file is an ~8B model.
        let b = estimate_params_b(16_000_000_000, "F16");
        assert!((7.0..9.0).contains(&b), "got {b}");
    }

    #[test]
    fn estimate_size_bytes_roundtrips_with_estimate_params_b() {
        for quant in ["Q4_K_M", "Q8_0", "F16", "Q2_K"] {
            let size = estimate_size_bytes(7.0, quant);
            let back = estimate_params_b(size, quant);
            assert!(
                (back - 7.0).abs() < 0.01,
                "{quant}: got {back} params back from {size} bytes"
            );
        }
    }

    // -- hub browser: repo-id/revision validation --------------------------

    #[test]
    fn validate_repo_id_accepts_normal_ids() {
        assert!(validate_repo_id("unsloth/Qwen3-4B-GGUF").is_ok());
        assert!(validate_repo_id("bartowski/Llama-3.2-1B.i1-GGUF").is_ok());
    }

    #[test]
    fn validate_repo_id_rejects_path_escapes_and_malformed_ids() {
        for bad in [
            "owner",
            "owner/..",
            "../etc/passwd",
            "owner/../secret",
            "owner/repo/extra",
            "ow ner/repo",
            "owner/.",
        ] {
            assert!(validate_repo_id(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn validate_revision_accepts_branches_and_refs() {
        assert!(validate_revision("main").is_ok());
        assert!(validate_revision("refs/pr/12").is_ok());
        assert!(validate_revision("a1b2c3d4").is_ok());
    }

    #[test]
    fn validate_revision_rejects_traversal_and_unsafe_chars() {
        for bad in [
            "",
            "../../etc",
            "main/../../etc",
            "/main",
            "main/",
            "main; rm -rf",
        ] {
            assert!(
                validate_revision(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    // -- hub browser: gated union / license tags ----------------------------

    #[test]
    fn gated_from_value_handles_the_bool_string_union() {
        assert!(!gated_from_value(&serde_json::Value::Bool(false)));
        assert!(gated_from_value(&serde_json::Value::Bool(true)));
        assert!(gated_from_value(&serde_json::Value::String(
            "auto".to_string()
        )));
        assert!(gated_from_value(&serde_json::Value::String(
            "manual".to_string()
        )));
        assert!(!gated_from_value(&serde_json::Value::String(
            "false".to_string()
        )));
        assert!(!gated_from_value(&serde_json::Value::Null));
    }

    #[test]
    fn license_from_tags_finds_the_license_prefixed_tag() {
        let tags = vec![
            "region:us".to_string(),
            "license:apache-2.0".to_string(),
            "gguf".to_string(),
        ];
        assert_eq!(license_from_tags(&tags), Some("apache-2.0".to_string()));
        assert_eq!(license_from_tags(&["gguf".to_string()]), None);
    }

    // -- hub browser: Link-header cursor parsing -----------------------------

    #[test]
    fn next_cursor_from_link_header_extracts_the_next_rel() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::LINK,
            "<https://huggingface.co/api/models?cursor=abc123>; rel=\"next\""
                .parse()
                .unwrap(),
        );
        assert_eq!(
            next_cursor_from_link_header(&headers),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn next_cursor_from_link_header_handles_multiple_rels_and_absence() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::LINK,
            "<https://huggingface.co/api/models?cursor=prev1>; rel=\"prev\", <https://huggingface.co/api/models?cursor=next1>; rel=\"next\""
                .parse()
                .unwrap(),
        );
        assert_eq!(
            next_cursor_from_link_header(&headers),
            Some("next1".to_string())
        );

        assert_eq!(
            next_cursor_from_link_header(&reqwest::header::HeaderMap::new()),
            None
        );
    }

    // -- hub browser: retry-after parsing -------------------------------------

    #[test]
    fn parse_retry_seconds_reads_the_t_field() {
        assert_eq!(parse_retry_seconds("\"api\";r=0;t=30"), Some(30));
        assert_eq!(parse_retry_seconds("t=5"), Some(5));
        assert_eq!(parse_retry_seconds("\"api\";r=0"), None);
    }

    // -- hub browser: README frontmatter stripping ---------------------------

    #[test]
    fn strip_frontmatter_removes_a_leading_yaml_block() {
        let md = "---\nlicense: apache-2.0\ntags:\n  - gguf\n---\n# Hello\n\nBody text.";
        assert_eq!(strip_frontmatter(md), "# Hello\n\nBody text.");
    }

    #[test]
    fn strip_frontmatter_leaves_content_without_frontmatter_unchanged() {
        let md = "# Hello\n\nNo frontmatter here.";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn strip_frontmatter_handles_frontmatter_only_files() {
        let md = "---\nlicense: mit\n---";
        assert_eq!(strip_frontmatter(md), "");
    }

    #[test]
    fn strip_frontmatter_leaves_unclosed_frontmatter_unchanged() {
        let md = "---\nlicense: mit\n# no closing delimiter";
        assert_eq!(strip_frontmatter(md), md);
    }

    // -- hub browser: README byte-cap truncation -----------------------------

    #[test]
    fn truncate_at_char_boundary_never_splits_a_multi_byte_char() {
        // "日" is 3 bytes (byte offsets 2..5); cutting anywhere inside it
        // must back off to before it, never split it.
        let s = "ab日cd";
        assert_eq!(truncate_at_char_boundary(s, 2), "ab");
        assert_eq!(truncate_at_char_boundary(s, 3), "ab");
        assert_eq!(truncate_at_char_boundary(s, 4), "ab");
        assert_eq!(truncate_at_char_boundary(s, 5), "ab日");
        assert_eq!(truncate_at_char_boundary(s, 100), s);
    }

    // -- hub browser: search URL construction --------------------------------

    #[test]
    fn search_query_url_includes_filter_sort_and_expand_fields() {
        let query = HfSearchQuery {
            search: Some("qwen".to_string()),
            author: None,
            pipeline_tag: Some("text-generation".to_string()),
            sort: HfSortKey::Downloads,
            cursor: None,
        };
        let url = build_search_url(&query).to_string();
        assert!(url.starts_with("https://huggingface.co/api/models?"));
        assert!(url.contains("filter=gguf"));
        assert!(url.contains("sort=downloads"));
        assert!(url.contains("direction=-1"));
        assert!(url.contains("search=qwen"));
        assert!(url.contains("pipeline_tag=text-generation"));
        for field in SEARCH_EXPAND_FIELDS {
            assert!(
                url.contains(&format!("expand%5B%5D={field}")),
                "missing expand[] for {field} in {url}"
            );
        }
    }

    #[test]
    fn search_query_url_omits_blank_optional_fields() {
        let query = HfSearchQuery::default();
        let url = build_search_url(&query).to_string();
        assert!(!url.contains("search="));
        assert!(!url.contains("author="));
        assert!(!url.contains("pipeline_tag="));
        assert!(!url.contains("cursor="));
    }

    // -- hub browser: search/detail response parsing (canned JSON) ----------

    #[test]
    fn parse_search_response_handles_items_with_and_without_optional_fields() {
        let body = r#"[
            {
                "id": "unsloth/Qwen3-4B-GGUF",
                "author": "unsloth",
                "pipeline_tag": "text-generation",
                "library_name": "gguf",
                "downloads": 12345,
                "likes": 42,
                "tags": ["gguf", "license:apache-2.0"],
                "gated": false,
                "lastModified": "2026-06-01T00:00:00.000Z",
                "gguf": { "total": 4022468096, "architecture": "qwen3", "context_length": 32768 }
            },
            {
                "id": "someorg/no-extras-GGUF",
                "tags": ["gguf"],
                "gated": "manual"
            }
        ]"#;
        let page = parse_search_response(body, Some("cursor123".to_string())).unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("cursor123"));
        assert_eq!(page.models.len(), 2);

        let full = &page.models[0];
        assert_eq!(full.repo_id, "unsloth/Qwen3-4B-GGUF");
        assert_eq!(full.author.as_deref(), Some("unsloth"));
        assert_eq!(full.library_name.as_deref(), Some("gguf"));
        assert_eq!(full.downloads, Some(12345));
        assert_eq!(full.license.as_deref(), Some("apache-2.0"));
        assert!(!full.gated);
        // Camelcase-only hub field — regression coverage for the rename bug
        // fixed alongside this test (see `parse_model_details_...` above).
        assert_eq!(
            full.last_modified.as_deref(),
            Some("2026-06-01T00:00:00.000Z")
        );
        assert_eq!(
            full.gguf.as_ref().and_then(|g| g.total_params),
            Some(4022468096)
        );
        // Fit is never computed by core — the command layer fills it in
        // once it has `HardwareInfo`.
        assert!(full.fit.is_none());
        assert!(full.fit_why.is_none());

        // Missing library_name/downloads/likes/author/lastModified/gguf —
        // every field beyond repoId/tags-derived license/gated is optional.
        let sparse = &page.models[1];
        assert_eq!(sparse.repo_id, "someorg/no-extras-GGUF");
        assert_eq!(sparse.author, None);
        assert_eq!(sparse.library_name, None);
        assert_eq!(sparse.downloads, None);
        assert_eq!(sparse.license, None);
        assert!(sparse.gated, "\"manual\" must be treated as gated");
        assert!(sparse.gguf.is_none());
    }

    #[test]
    fn parse_search_response_rejects_unparseable_json() {
        assert!(parse_search_response("not json", None).is_err());
    }

    #[test]
    fn parse_model_details_reads_gguf_metadata_and_card_license() {
        let body = r#"{
            "id": "bartowski/Llama-3.2-1B-Instruct-GGUF",
            "author": "bartowski",
            "pipeline_tag": "text-generation",
            "downloads": 999,
            "likes": 10,
            "tags": ["gguf", "license:llama3.2"],
            "gated": false,
            "lastModified": "2026-05-01T00:00:00.000Z",
            "createdAt": "2024-09-25T00:00:00.000Z",
            "cardData": { "license": "llama3.2" },
            "gguf": { "total": 1235814432, "architecture": "llama", "context_length": 131072 }
        }"#;
        let details = parse_model_details(body).unwrap();
        assert_eq!(details.repo_id, "bartowski/Llama-3.2-1B-Instruct-GGUF");
        assert_eq!(details.license.as_deref(), Some("llama3.2"));
        assert!(!details.gated);
        // Both camelCase-only hub fields — regression coverage for the
        // rename bug fixed alongside this test.
        assert_eq!(
            details.last_modified.as_deref(),
            Some("2026-05-01T00:00:00.000Z")
        );
        assert_eq!(
            details.created_at.as_deref(),
            Some("2024-09-25T00:00:00.000Z")
        );
        let gguf = details.gguf.expect("gguf metadata present");
        assert_eq!(gguf.total_params, Some(1235814432));
        assert_eq!(gguf.architecture.as_deref(), Some("llama"));
        assert_eq!(gguf.context_length, Some(131072));
    }

    #[test]
    fn parse_model_details_falls_back_to_tags_when_card_data_has_no_license() {
        let body = r#"{
            "id": "someorg/model-GGUF",
            "tags": ["gguf", "license:mit"],
            "gated": "auto"
        }"#;
        let details = parse_model_details(body).unwrap();
        assert_eq!(details.license.as_deref(), Some("mit"));
        assert!(details.gated);
        assert!(details.gguf.is_none());
    }

    // -- hub browser: live network tests (run with `-- --ignored`) ----------

    /// Network test against the real Hugging Face API; run explicitly with
    /// `cargo test -p openpal-core huggingface -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn searches_real_trending_gguf_models() {
        let query = HfSearchQuery::default();
        let page = search_models(&reqwest::Client::new(), &query)
            .await
            .unwrap();
        assert!(
            !page.models.is_empty(),
            "trending GGUF search should return results"
        );
        assert!(page.models.iter().all(|m| !m.repo_id.is_empty()));
    }

    /// Network test against the real Hugging Face API; run explicitly with
    /// `cargo test -p openpal-core huggingface -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetches_real_model_details_and_readme() {
        let repo_id = "lmstudio-community/SmolLM2-135M-Instruct-GGUF";
        let details = model_details(&reqwest::Client::new(), repo_id)
            .await
            .unwrap();
        assert_eq!(details.repo_id, repo_id);

        let readme = fetch_readme(&reqwest::Client::new(), repo_id, "main")
            .await
            .unwrap();
        assert!(!readme.trim().is_empty());
        assert!(
            !readme.trim_start().starts_with("---"),
            "frontmatter should be stripped"
        );
    }
}
