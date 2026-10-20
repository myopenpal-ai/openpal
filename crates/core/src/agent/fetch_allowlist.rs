//! Which URLs `fetch_url` may open this turn, and how many it may open.
//!
//! `fetch_url`'s target is written by the model, and text the model has read
//! (a fetched page, an attached file) can steer it into putting the user's
//! words into a URL of an attacker's choosing — a GET that leaks the
//! conversation with no click. So a URL may be fetched only when it exactly
//! matches, fragment aside, one the turn got from somewhere the model can't
//! write: the user's own messages (typed text, invoked skill bodies,
//! document attachments), the user's system prompt, a free-typed `ask_user`
//! answer, a `web_search` result, or an earlier answer's citations. Text the
//! model or a tool wrote never counts — not the assistant's own replies, not
//! a fetched page's links, not a `spawn_subagent` helper's `task`.
//!
//! One allowlist lives on [`super::AgentDeps`], which a helper clones, so the
//! `web-research` helper shares both the list and the fetch budget with the
//! turn that spawned it.

use crate::types::ChatMessage;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// `fetch_url` calls one top-level turn may make, helpers included.
pub const MAX_FETCHES_PER_TURN: u32 = 10;

/// Characters that end a URL found in free text. None of them may appear
/// unescaped in a URL.
const URL_TERMINATORS: &[char] = &['<', '>', '"', '`', '{', '}', '|', '\\', '^'];

/// Trailing characters that are usually prose around a URL rather than part
/// of it ("see https://example.com/a." or "[a](https://example.com/a)").
const TRAILING_PUNCTUATION: &[char] = &[
    '.', ',', ';', ':', '!', '?', '\'', '"', ')', ']', '}', '*', '_', '~',
];

pub struct FetchAllowlist {
    urls: Mutex<HashSet<String>>,
    fetches: AtomicU32,
}

impl FetchAllowlist {
    pub fn new() -> Self {
        Self {
            urls: Mutex::new(HashSet::new()),
            fetches: AtomicU32::new(0),
        }
    }

    /// Allow the URLs the turn's history carries: everything in the user's
    /// and system messages (attachment text included), and the citations of
    /// earlier answers — never an answer's own text. Only the messages the
    /// model reads count: one the user withheld after a prompt-injection
    /// review, and the replies after it, add nothing (`engine::model_bound`).
    pub fn allow_history(&self, messages: &[ChatMessage]) {
        for message in crate::engine::model_bound(messages) {
            match message.role.as_str() {
                "user" | "system" => {
                    self.allow_text(&message.content);
                    for attachment in message.attachments.iter().flatten() {
                        if let Some(text) = &attachment.text {
                            self.allow_text(text);
                        }
                    }
                }
                "assistant" => {
                    for citation in message.citations.iter().flatten() {
                        self.allow_url(&citation.url);
                    }
                }
                _ => {}
            }
        }
    }

    /// Allow every `http(s)://` URL written out in `text`.
    pub fn allow_text(&self, text: &str) {
        let mut urls = self.urls.lock().unwrap();
        for found in urls_in(text) {
            if let Some(key) = canonical(found) {
                urls.insert(key);
            }
        }
    }

    pub fn allow_url(&self, url: &str) {
        if let Some(key) = canonical(url) {
            self.urls.lock().unwrap().insert(key);
        }
    }

    pub fn is_allowed(&self, url: &str) -> bool {
        canonical(url).is_some_and(|key| self.urls.lock().unwrap().contains(&key))
    }

    /// Count one fetch against the turn's budget. `false` once
    /// [`MAX_FETCHES_PER_TURN`] have been made.
    pub fn take_fetch(&self) -> bool {
        self.fetches.fetch_add(1, Ordering::SeqCst) < MAX_FETCHES_PER_TURN
    }
}

impl Default for FetchAllowlist {
    fn default() -> Self {
        Self::new()
    }
}

/// The form two URLs are compared in: parsed and re-serialised (so letter
/// case in the host, or a missing `/` path, doesn't matter) with the fragment
/// dropped, since a fragment never leaves the machine. Anything else the
/// model adds — a query parameter, a path segment, a subdomain, a `user@` —
/// changes the result, so it no longer matches.
fn canonical(url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.into())
}

/// Every `http://`/`https://` URL in `text`, each also with its trailing
/// punctuation peeled off one character at a time — the URL as written and
/// every shorter reading are all things the user wrote, so offering each
/// lets a link wrapped in prose or Markdown still match.
fn urls_in(text: &str) -> Vec<&str> {
    // ASCII lowercasing keeps every byte offset the same as in `text`.
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("http") {
        let start = from + offset;
        let rest = &lower[start..];
        if !rest.starts_with("http://") && !rest.starts_with("https://") {
            from = start + "http".len();
            continue;
        }
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || c.is_control() || URL_TERMINATORS.contains(&c))
            .map_or(text.len(), |len| start + len);
        let mut url = &text[start..end];
        found.push(url);
        while let Some(last) = url
            .chars()
            .last()
            .filter(|c| TRAILING_PUNCTUATION.contains(c))
        {
            url = &url[..url.len() - last.len_utf8()];
            found.push(url);
        }
        from = end;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Attachment, AttachmentKind, Citation};

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: crate::types::new_message_id(),
            role: role.to_string(),
            content: content.to_string(),
            created_at_ms: None,
            model: None,
            citations: None,
            thinking: None,
            steps: None,
            attachments: None,
            artifacts: None,
            stats: None,
            display_content: None,
            injection_notices: None,
            withheld_from_model: None,
        }
    }

    fn allowing(text: &str) -> FetchAllowlist {
        let list = FetchAllowlist::new();
        list.allow_text(text);
        list
    }

    #[test]
    fn a_url_in_the_text_is_allowed_exactly() {
        let list = allowing("Summarize https://example.com/news/today?id=7 please");
        assert!(list.is_allowed("https://example.com/news/today?id=7"));
    }

    #[test]
    fn the_fragment_and_host_case_do_not_matter() {
        let list = allowing("https://Example.com/a#intro");
        assert!(list.is_allowed("https://example.com/a"));
        assert!(list.is_allowed("https://example.com/a#elsewhere"));
    }

    #[test]
    fn anything_added_to_an_allowed_url_is_refused() {
        let list = allowing("https://example.com/a?id=7");
        assert!(!list.is_allowed("https://example.com/a?id=7&d=secret"));
        assert!(!list.is_allowed("https://example.com/a/secret?id=7"));
        assert!(!list.is_allowed("https://secret.example.com/a?id=7"));
        assert!(!list.is_allowed("https://secret@example.com/a?id=7"));
        assert!(!list.is_allowed("https://example.com.secret.net/a?id=7"));
    }

    #[test]
    fn a_shorter_url_than_the_one_written_is_refused() {
        let list = allowing("https://example.com/a/b");
        assert!(!list.is_allowed("https://example.com/a"));
    }

    #[test]
    fn surrounding_prose_and_markdown_are_peeled_off() {
        let list = allowing(
            "See https://example.com/a. Also [docs](https://example.com/b), \
             <https://example.com/c> and \"https://example.com/d\"",
        );
        for path in ["a", "b", "c", "d"] {
            assert!(
                list.is_allowed(&format!("https://example.com/{path}")),
                "{path}"
            );
        }
    }

    #[test]
    fn a_url_that_ends_in_a_parenthesis_keeps_it() {
        let list = allowing("https://en.wikipedia.org/wiki/Rust_(programming_language)");
        assert!(list.is_allowed("https://en.wikipedia.org/wiki/Rust_(programming_language)"));
    }

    #[test]
    fn non_web_schemes_are_never_allowed() {
        let list = FetchAllowlist::new();
        list.allow_url("file:///etc/passwd");
        assert!(!list.is_allowed("file:///etc/passwd"));
    }

    #[test]
    fn history_allows_user_system_attachment_and_citation_urls_only() {
        let mut user = message("user", "Read https://example.com/typed");
        user.attachments = Some(vec![Attachment {
            kind: AttachmentKind::Document,
            name: "notes.txt".into(),
            data_uri: None,
            text: Some("https://example.com/attached".into()),
        }]);
        let mut answer = message("assistant", "I also found https://example.com/written");
        answer.citations = Some(vec![Citation {
            id: 1,
            url: "https://example.com/cited".into(),
            title: "Cited".into(),
        }]);
        let list = FetchAllowlist::new();
        list.allow_history(&[
            message("system", "Prefer https://example.com/system"),
            user,
            answer,
        ]);

        for path in ["typed", "attached", "system", "cited"] {
            assert!(
                list.is_allowed(&format!("https://example.com/{path}")),
                "{path}"
            );
        }
        assert!(!list.is_allowed("https://example.com/written"));
    }

    #[test]
    fn a_withheld_message_and_its_replies_allow_nothing() {
        let mut withheld = message("user", "Read https://example.com/withheld");
        withheld.withheld_from_model = Some(true);
        let mut answer = message("assistant", "Done.");
        answer.citations = Some(vec![Citation {
            id: 1,
            url: "https://example.com/cited".into(),
            title: "Cited".into(),
        }]);
        let list = FetchAllowlist::new();
        list.allow_history(&[
            withheld,
            answer,
            message("user", "Now https://example.com/kept"),
        ]);

        assert!(!list.is_allowed("https://example.com/withheld"));
        assert!(!list.is_allowed("https://example.com/cited"));
        assert!(list.is_allowed("https://example.com/kept"));
    }

    #[test]
    fn the_budget_runs_out_after_ten_fetches() {
        let list = FetchAllowlist::new();
        for _ in 0..MAX_FETCHES_PER_TURN {
            assert!(list.take_fetch());
        }
        assert!(!list.take_fetch());
    }
}
