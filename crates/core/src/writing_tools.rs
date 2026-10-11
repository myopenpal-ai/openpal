//! Pure request-body builders for the Writing Tools plugin's Translate and
//! Correct commands (`writing-tools` capability, `plugins/writing-tools`) —
//! the translate/correct twin of `crate::engine`'s `build_classify_body`/
//! `build_risk_classify_body`: pure functions, no I/O, unit-testable without
//! a live server. Unlike those, these back a foreground, user-initiated,
//! *streamed* free-text completion rather than classify's background,
//! non-streaming, single-label probe.
//!
//! Deliberately has no dependency on `crate::settings` at all — in
//! particular, the user's chat-persona system prompt (a free-text `Settings`
//! field) must never leak into a translate/correct request. Each mode below
//! hardcodes its own fixed system prompt from scratch and its own fixed,
//! non-tunable [`SamplingParams`] preset close to `TaskType::Coding`'s
//! (temperature 0.2 / top_p 0.9 / top_k 20 — `general_sampling_defaults` in
//! `crate::types` already notes, in its own doc comment, that this is the
//! right determinism target for translation-like output). That preset is
//! redefined locally rather than reused via
//! `general_sampling_defaults(TaskType::Coding)`, so a future retune of
//! chat's own Coding defaults can't silently drag this tool's output along
//! with it.
//!
//! Unlike `build_classify_body`/`build_risk_classify_body`, no `grammar`
//! field is used here: those constrain output to exactly one label from a
//! closed set, which is exactly what a GBNF rule is for; translate/correct
//! produce free-form text of unknown length, which a single-choice grammar
//! can't express.
//!
//! Returns plain system-prompt strings and a `SamplingParams` value, not a
//! merged request body — `EngineManager::chat_stream` and
//! `openpal_core::cloud::chat_stream` both take `messages: &[ChatMessage]`
//! plus `&SamplingParams` as separate arguments, so the command layer
//! (`crates/app/src/commands.rs`) assembles the actual `Vec<ChatMessage>`
//! itself and passes this module's `SamplingParams` straight through.

use crate::types::SamplingParams;

/// One entry in the curated language list Translate mode's dropdowns are
/// built from. A small, hand-picked catalog (roughly 20-30 languages), not
/// "every language the base model has ever seen a token of" — the same
/// curated-over-exhaustive posture as `crate::catalog`, and honest about
/// what a 4-13B local model actually translates well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Language {
    /// Stable wire id (lowercase ISO 639-1 where one exists) — what the
    /// frontend's dropdowns send as `source_lang`/`target_lang`.
    pub id: &'static str,
    /// English display name, substituted into the translate system prompt
    /// and shown in the dropdown.
    pub name: &'static str,
}

/// The curated language list. Content-only; growing it later is not a
/// structural change to this module — see "Open questions" in
/// `docs/design/WRITING_TOOLS.md` on how membership should eventually be decided.
pub const LANGUAGES: &[Language] = &[
    Language {
        id: "en",
        name: "English",
    },
    Language {
        id: "es",
        name: "Spanish",
    },
    Language {
        id: "fr",
        name: "French",
    },
    Language {
        id: "de",
        name: "German",
    },
    Language {
        id: "it",
        name: "Italian",
    },
    Language {
        id: "pt",
        name: "Portuguese",
    },
    Language {
        id: "nl",
        name: "Dutch",
    },
    Language {
        id: "sv",
        name: "Swedish",
    },
    Language {
        id: "no",
        name: "Norwegian",
    },
    Language {
        id: "da",
        name: "Danish",
    },
    Language {
        id: "fi",
        name: "Finnish",
    },
    Language {
        id: "pl",
        name: "Polish",
    },
    Language {
        id: "cs",
        name: "Czech",
    },
    Language {
        id: "ro",
        name: "Romanian",
    },
    Language {
        id: "hu",
        name: "Hungarian",
    },
    Language {
        id: "el",
        name: "Greek",
    },
    Language {
        id: "ru",
        name: "Russian",
    },
    Language {
        id: "uk",
        name: "Ukrainian",
    },
    Language {
        id: "tr",
        name: "Turkish",
    },
    Language {
        id: "ar",
        name: "Arabic",
    },
    Language {
        id: "he",
        name: "Hebrew",
    },
    Language {
        id: "hi",
        name: "Hindi",
    },
    Language {
        id: "vi",
        name: "Vietnamese",
    },
    Language {
        id: "th",
        name: "Thai",
    },
    Language {
        id: "id",
        name: "Indonesian",
    },
    Language {
        id: "zh",
        name: "Chinese (Simplified)",
    },
    Language {
        id: "ja",
        name: "Japanese",
    },
    Language {
        id: "ko",
        name: "Korean",
    },
];

/// Look up a curated language's display name by its wire id. `None` when
/// `id` isn't in [`LANGUAGES`] — callers treat that as "use the raw id
/// verbatim" rather than a hard failure, so a stale id from an older
/// frontend build degrades gracefully instead of erroring.
pub fn language_name(id: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|l| l.id == id).map(|l| l.name)
}

/// Fixed sampling preset shared by both Translate and Correct: close to
/// `TaskType::Coding`'s determinism (temperature 0.2 / top_p 0.9 / top_k 20),
/// never exposed to the user as tunable sliders — Writing Tools bypasses the
/// Generation panel entirely. `pub`: the command layer passes this straight
/// to `EngineManager::chat_stream`/`cloud::chat_stream` as `&SamplingParams`.
pub fn fixed_sampling() -> SamplingParams {
    SamplingParams {
        temperature: 0.2,
        top_p: 0.9,
        top_k: 20,
        ..SamplingParams::default()
    }
}

/// Build the fixed system prompt for a streamed translate completion. Pure
/// (no I/O), unit-testable without a live server, and returns the prompt
/// text only — the command layer wraps it into a `ChatMessage` itself,
/// alongside the user's text and [`fixed_sampling`].
///
/// `target_lang` and `source_lang` are wire ids from [`LANGUAGES`] (e.g.
/// `"fr"`); `source_lang: None` means "detect" — the prompt simply omits the
/// source-language clause instead of a separate detection round trip.
pub fn translate_system_prompt(source_lang: Option<&str>, target_lang: &str) -> String {
    let target = language_name(target_lang).unwrap_or(target_lang);
    match source_lang.map(|id| language_name(id).unwrap_or(id)) {
        Some(source) => format!(
            "You are a translation engine embedded in a writing tool. \
             Translate the user's text from {source} into {target}. \
             Preserve the original meaning, tone, and formatting (line \
             breaks, lists, punctuation). Output only the translated text \
             in {target} — no explanations, no notes, no quotation marks, \
             and do not restate the original."
        ),
        None => format!(
            "You are a translation engine embedded in a writing tool. \
             Translate the user's text into {target}. Preserve the \
             original meaning, tone, and formatting (line breaks, lists, \
             punctuation). Output only the translated text in {target} — \
             no explanations, no notes, no quotation marks, and do not \
             restate the original."
        ),
    }
}

/// Fixed system prompt for Correct mode. A `const` so its exact wording is a
/// single, greppable, unit-tested source of truth.
pub const CORRECT_SYSTEM_PROMPT: &str = "You are a grammar and style correction \
    engine embedded in a writing tool. Fix grammar, spelling, and \
    punctuation mistakes in the user's text, and improve awkward phrasing, \
    without changing its meaning or the language it is written in. Output \
    only the corrected text — no explanations, no notes, no quotation \
    marks, and do not comment on what you changed.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_prompt_names_source_and_target_languages() {
        let system = translate_system_prompt(Some("en"), "fr");
        assert!(
            system.contains("English"),
            "should name the source language: {system}"
        );
        assert!(
            system.contains("French"),
            "should name the target language: {system}"
        );
    }

    #[test]
    fn translate_prompt_omits_source_clause_when_detecting() {
        let system = translate_system_prompt(None, "fr");
        assert!(
            !system.contains(" from "),
            "detect mode should not name a source language: {system}"
        );
        assert!(system.contains("French"));
    }

    #[test]
    fn translate_prompt_falls_back_to_the_raw_id_for_an_unknown_language() {
        let system = translate_system_prompt(None, "xx-unknown");
        assert!(system.contains("xx-unknown"));
    }

    #[test]
    fn correct_prompt_preserves_meaning_and_language() {
        assert!(
            CORRECT_SYSTEM_PROMPT.contains("without changing"),
            "must instruct the model to preserve meaning/language"
        );
    }

    #[test]
    fn fixed_sampling_matches_codings_low_temperature_determinism() {
        let s = fixed_sampling();
        assert_eq!(s.temperature, 0.2);
        assert_eq!(s.top_p, 0.9);
        assert_eq!(s.top_k, 20);
    }

    #[test]
    fn every_curated_language_has_a_non_empty_id_and_name() {
        for lang in LANGUAGES {
            assert!(!lang.id.is_empty());
            assert!(!lang.name.is_empty());
        }
    }

    #[test]
    fn language_ids_are_unique() {
        let mut ids: Vec<&str> = LANGUAGES.iter().map(|l| l.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            LANGUAGES.len(),
            "duplicate language id in LANGUAGES"
        );
    }

    /// Guards the risk register's top concern for this module: a translate
    /// or correct request must never carry the user's chat-persona system
    /// prompt setting. This module never imports `crate::settings` in the
    /// first place, so there is no value in scope to leak — this test
    /// enforces that structurally, by scanning this file's own source for a
    /// field-access pattern that would indicate such a leak: a dot followed
    /// immediately by that persona field's two-word snake_case name. The
    /// needle is built at runtime from two halves and deliberately
    /// dot-prefixed, both so this comment can describe the check without
    /// spelling out (and thereby tripping) the exact needle, and so it does
    /// not match this file's own translate-prompt function name (an
    /// underscore there, not a field access).
    #[test]
    fn this_module_never_references_the_user_chat_persona_field() {
        let forbidden = format!(".{}{}", "system_", "prompt");
        let source = include_str!("writing_tools.rs");
        assert!(
            !source.contains(&forbidden),
            "writing_tools.rs must never access Settings' free-text persona field"
        );
    }
}
