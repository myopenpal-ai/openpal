//! What a notification says by the time it reaches a backend: capped in
//! length, and the body escaped where the OS may read it as markup. Pure, so
//! every rule here is tested on any host.

use std::borrow::Cow;

/// Longest title / body handed to the OS, in characters. An alert's copy is a
/// short fixed sentence plus a model's display name, so these are generous;
/// the caps exist so the one command that can raise an OS notification can
/// never be made to push a wall of text at a notification server.
pub(super) const MAX_TITLE_CHARS: usize = 120;
pub(super) const MAX_BODY_CHARS: usize = 300;

/// The title and body as they are handed to the OS. `escape_body_markup` is
/// the backend's own answer (see `Notifier::renders_body_markup`).
///
/// Truncation comes first so an escape sequence is never cut in half.
pub(super) fn prepare(title: &str, body: &str, escape_body_markup: bool) -> (String, String) {
    let title = truncate_chars(title, MAX_TITLE_CHARS).into_owned();
    let body = truncate_chars(body, MAX_BODY_CHARS);
    let body = if escape_body_markup {
        escape_markup(&body)
    } else {
        body.into_owned()
    };
    (title, body)
}

/// Cut `s` to at most `max` characters (not bytes, so a multi-byte character
/// is never split), ending in an ellipsis when something was removed.
fn truncate_chars(s: &str, max: usize) -> Cow<'_, str> {
    if s.chars().count() <= max {
        return Cow::Borrowed(s);
    }
    let mut cut: String = s.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    Cow::Owned(cut)
}

/// Escape the three characters a freedesktop notification server may read as
/// markup. Linux only: the spec lets a server render `<b>`, `<a>` and friends
/// in the body, and some do, so a model name containing `<` or `&` has to be
/// inert. macOS and Windows show the body as plain text, where escaping would
/// print the entities literally.
fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_and_marks_the_cut() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("exactly10!", 10), "exactly10!");
        let cut = truncate_chars("0123456789ABC", 10);
        assert_eq!(cut, "012345678…");
        assert_eq!(cut.chars().count(), 10);
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // 2-byte and 4-byte characters: a byte-indexed cut would panic.
        let cut = truncate_chars("éééééééé", 4);
        assert_eq!(cut, "ééé…");
        let cut = truncate_chars("🙂🙂🙂🙂🙂", 3);
        assert_eq!(cut, "🙂🙂…");
    }

    #[test]
    fn markup_characters_are_escaped_once() {
        assert_eq!(
            escape_markup("Qwen <b>3</b> & friends"),
            "Qwen &lt;b&gt;3&lt;/b&gt; &amp; friends"
        );
        assert_eq!(escape_markup("plain text"), "plain text");
        // `&` is escaped on its own, never double-escaped by a later step.
        assert_eq!(escape_markup("&lt;"), "&amp;lt;");
    }

    #[test]
    fn only_the_body_is_escaped_and_only_where_asked() {
        let (title, body) = prepare("A & B", "x < y", true);
        assert_eq!(title, "A & B");
        assert_eq!(body, "x &lt; y");

        let (title, body) = prepare("A & B", "x < y", false);
        assert_eq!(title, "A & B");
        assert_eq!(body, "x < y");
    }

    #[test]
    fn title_and_body_are_capped_before_they_reach_the_os() {
        let long_title = "t".repeat(500);
        let long_body = "b".repeat(5000);
        let (title, body) = prepare(&long_title, &long_body, false);
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(body.chars().count(), MAX_BODY_CHARS);
        assert!(title.ends_with('…') && body.ends_with('…'));
    }

    #[test]
    fn truncation_happens_before_escaping_so_an_entity_is_never_cut() {
        // The "<" is the 299th character, so it survives the cap and is then
        // expanded whole. Escaping first would push it past the cap and the
        // cut could land inside "&lt;".
        let body = format!("{}<tail", "a".repeat(MAX_BODY_CHARS - 2));
        let (_, out) = prepare("t", &body, true);
        assert!(out.ends_with("&lt;…"), "{out:?}");
    }
}
