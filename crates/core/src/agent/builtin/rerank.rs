//! Relevance-score a fetched page's chunks against the query that motivated
//! the fetch, so a long page's most useful excerpts reach the model instead
//! of just its first N characters.
//!
//! The Python sidecar this replaces used a cross-encoder (`bge-reranker-v2-m3`
//! via FlagEmbedding, or `sentence_transformers.CrossEncoder` as a fallback)
//! — a second neural model loaded alongside the chat model. That's exactly
//! what CLAUDE.md's "mono-model first" rule rules out on the default path:
//! OpenPal loads one model and uses it for everything, not a second
//! specialized model even where that second model would score better.
//!
//! BM25 needs no extra model and no extra runtime dependency: it's a
//! closed-form formula over term frequencies, scored against the fetched
//! page's own chunks as its corpus (so a term's IDF reflects how rare it is
//! *within this page*, not some global corpus). Weaker than a cross-encoder
//! at genuine semantic matching (synonyms, paraphrase), but it correctly
//! rewards chunks that actually mention the query's distinctive terms, which
//! is most of what relevance ranking needs here.

use std::collections::HashMap;

const BM25_K1: f32 = 1.5;
const BM25_B: f32 = 0.75;

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn score_all(query: &str, chunks: &[String]) -> Vec<f32> {
    let query_terms = tokenize(query);
    let docs: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(c)).collect();
    let n = docs.len() as f32;
    let avg_len = if docs.is_empty() {
        0.0
    } else {
        docs.iter().map(|d| d.len() as f32).sum::<f32>() / n
    };

    let mut df: HashMap<&str, f32> = HashMap::new();
    for term in &query_terms {
        let count = docs.iter().filter(|d| d.iter().any(|t| t == term)).count() as f32;
        df.insert(term.as_str(), count);
    }

    docs.iter()
        .map(|doc| {
            let doc_len = doc.len() as f32;
            let mut tf_counts: HashMap<&str, f32> = HashMap::new();
            for t in doc {
                *tf_counts.entry(t.as_str()).or_insert(0.0) += 1.0;
            }
            query_terms
                .iter()
                .map(|term| {
                    let tf = tf_counts.get(term.as_str()).copied().unwrap_or(0.0);
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let d = df[term.as_str()];
                    let idf = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
                    let denom = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len / avg_len.max(1.0));
                    idf * (tf * (BM25_K1 + 1.0)) / denom
                })
                .sum()
        })
        .collect()
}

/// Pick the `k` chunks most relevant to `query`, highest-scoring first. Falls
/// back to original document order when `query` shares no terms with any
/// chunk (score-all-zero) — better to hand over the page's start than an
/// arbitrary/unstable ordering.
pub(crate) fn top_chunks(query: &str, chunks: Vec<String>, k: usize) -> Vec<String> {
    if chunks.len() <= k {
        return chunks;
    }
    let scores = score_all(query, &chunks);
    if scores.iter().all(|&s| s == 0.0) {
        return chunks.into_iter().take(k).collect();
    }
    let mut ranked: Vec<(usize, f32)> = scores.into_iter().enumerate().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(k)
        .map(|(i, _)| chunks[i].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_the_chunk_that_mentions_the_query_terms_first() {
        let chunks = vec![
            "The weather today is sunny with a light breeze.".to_string(),
            "Rust's ownership model prevents data races at compile time.".to_string(),
            "A recipe for banana bread: mash the bananas, mix in flour and sugar.".to_string(),
        ];
        let top = top_chunks("rust ownership data races", chunks, 1);
        assert_eq!(
            top,
            vec!["Rust's ownership model prevents data races at compile time.".to_string()]
        );
    }

    #[test]
    fn falls_back_to_original_order_when_nothing_matches() {
        let chunks = vec![
            "alpha beta".to_string(),
            "gamma delta".to_string(),
            "epsilon zeta".to_string(),
        ];
        let top = top_chunks("nonexistent query terms", chunks.clone(), 2);
        assert_eq!(top, vec![chunks[0].clone(), chunks[1].clone()]);
    }

    #[test]
    fn returns_everything_when_k_covers_all_chunks() {
        let chunks = vec!["one".to_string(), "two".to_string()];
        let top = top_chunks("anything", chunks.clone(), 5);
        assert_eq!(top, chunks);
    }
}
