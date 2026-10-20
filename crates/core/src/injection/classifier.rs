//! The classifier itself: the pinned model's `tokenizer.json` plus its
//! quantized ONNX graph, run on the CPU with tract. Pure, blocking, and
//! stateless per call — [`super::Detector`] owns when it's loaded, how often
//! it runs, and what happens to the score.

use super::model;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokenizers::{Encoding, Token, Tokenizer};
use tract_onnx::prelude::*;

/// Anything that can score text: the real [`OnnxClassifier`], or a fake in
/// tests. Blocking and CPU-bound — call it from `spawn_blocking`.
pub trait Classify: Send + Sync {
    /// P(injection) for `text`: the highest score of any window of it.
    fn p_injection(&self, text: &str) -> Result<f32, String>;

    /// [`p_injection`](Self::p_injection), plus where in `text` (a byte range
    /// on char boundaries) the highest-scoring window sits — what the review
    /// card shows, since the start of a long page usually isn't the part that
    /// tripped the check. `None` when there's no such range to give.
    fn p_injection_located(&self, text: &str) -> Result<(f32, Option<Range<usize>>), String> {
        self.p_injection(text).map(|p| (p, None))
    }
}

/// Tokens per forward pass, special tokens included. The model reads up to
/// 8,192, but 512 is what it was evaluated at for this app (see
/// `tests/fixtures/prompt_injection/README.md`): longer windows dilute a short
/// payload buried in a long page.
const WINDOW: usize = 512;
/// Tokens shared by neighbouring windows, so a payload straddling a window
/// boundary is still seen whole by one of them.
const OVERLAP: usize = 64;
/// `config.json`'s `id2label` for the pinned revision: `0: SAFE, 1: INJECTION`.
const INJECTION_INDEX: usize = 1;

pub struct OnnxClassifier {
    tokenizer: Tokenizer,
    plan: Arc<TypedRunnableModel>,
    /// Graph input index of `input_ids` and of `attention_mask`.
    input_ids_ix: usize,
    attention_mask_ix: usize,
}

impl OnnxClassifier {
    /// Load the pinned files from `dir` (see [`model::provision`]). Measured
    /// on an M4: 3.2–3.7 s with the files in the disk cache, about 14.5 s
    /// from a cold disk. The result is reused until [`super::Detector`]
    /// unloads it.
    pub fn load(dir: &Path) -> Result<Self, String> {
        configure_threads();

        let mut tokenizer = Tokenizer::from_file(dir.join(model::TOKENIZER_FILE))
            .map_err(|e| format!("tokenizer: {e}"))?;
        // tokenizer.json ships its training-time truncation; windowing is ours.
        tokenizer
            .with_truncation(None)
            .map_err(|e| format!("tokenizer: {e}"))?;
        tokenizer.with_padding(None);

        // The export names its dims `batch`/`sequence` in a way tract can't
        // unify through the graph's Flatten/Cast nodes, so its own shape
        // annotations are ignored and both inputs are pinned to `[1, S]`
        // instead: one plan serves every window length.
        let mut graph = tract_onnx::onnx()
            .with_ignore_value_info(true)
            .with_ignore_output_shapes(true)
            .model_for_path(dir.join(model::MODEL_FILE))
            .map_err(|e| format!("model: {e}"))?;
        let seq = graph.sym("S").to_dim();
        let input_names: Vec<String> = graph
            .input_outlets()
            .map_err(|e| format!("model: {e}"))?
            .iter()
            .map(|outlet| graph.node(outlet.node).name.clone())
            .collect();
        let position = |name: &str| {
            input_names
                .iter()
                .position(|n| n == name)
                .ok_or_else(|| format!("model: no `{name}` input (has {input_names:?})"))
        };
        if input_names.len() != 2 {
            return Err(format!(
                "model: expected input_ids and attention_mask, has {input_names:?}"
            ));
        }
        let input_ids_ix = position("input_ids")?;
        let attention_mask_ix = position("attention_mask")?;
        for ix in 0..input_names.len() {
            graph
                .set_input_fact(
                    ix,
                    InferenceFact::dt_shape(i64::datum_type(), tvec![1.to_dim(), seq.clone()]),
                )
                .map_err(|e| format!("model: {e}"))?;
        }
        let plan = graph
            .into_optimized()
            .and_then(|m| m.into_runnable())
            .map_err(|e| format!("model: {e:#}"))?;

        Ok(Self {
            tokenizer,
            plan,
            input_ids_ix,
            attention_mask_ix,
        })
    }

    /// One forward pass over `ids` (already wrapped in the special tokens).
    fn window_score(&self, ids: &[u32]) -> Result<f32, String> {
        let n = ids.len();
        let input_ids: Vec<i64> = ids.iter().map(|&id| i64::from(id)).collect();
        let input_ids: TValue = Tensor::from_shape(&[1, n], &input_ids)
            .map_err(|e| e.to_string())?
            .into();
        let attention_mask: TValue = Tensor::from_shape(&[1, n], &vec![1i64; n])
            .map_err(|e| e.to_string())?
            .into();
        let inputs: TVec<TValue> = if self.input_ids_ix < self.attention_mask_ix {
            tvec![input_ids, attention_mask]
        } else {
            tvec![attention_mask, input_ids]
        };
        let outputs = self.plan.run(inputs).map_err(|e| format!("{e:#}"))?;
        let logits = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(|e| e.to_string())?;
        let logits: Vec<f32> = logits.iter().copied().collect();
        softmax_at(&logits, INJECTION_INDEX)
            .ok_or_else(|| format!("unexpected classifier output {logits:?}"))
    }

    /// `ids` wrapped in the model's special tokens, exactly as the tokenizer's
    /// own post-processor adds them to a whole encoded text.
    fn wrap(&self, ids: &[u32]) -> Result<Vec<u32>, String> {
        let tokens = ids
            .iter()
            .map(|&id| {
                let value = self.tokenizer.id_to_token(id).unwrap_or_default();
                Token::new(id, value, (0, 0))
            })
            .collect();
        let encoding = self
            .tokenizer
            .post_process(Encoding::from_tokens(tokens, 0), None, true)
            .map_err(|e| e.to_string())?;
        Ok(encoding.get_ids().to_vec())
    }
}

impl Classify for OnnxClassifier {
    fn p_injection(&self, text: &str) -> Result<f32, String> {
        self.p_injection_located(text).map(|(p, _)| p)
    }

    /// About 0.3 s per window on an M4: a 20,000-character page takes about
    /// 3.4 s, a 200 KB tool result (the `MAX_TOOL_OUTPUT_BYTES` cap) 34 s.
    fn p_injection_located(&self, text: &str) -> Result<(f32, Option<Range<usize>>), String> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| e.to_string())?;
        let ids = encoding.get_ids();
        let body = WINDOW - 2;
        let mut best: Option<(f32, Range<usize>)> = None;
        for start in window_starts(ids.len(), body, OVERLAP) {
            let end = (start + body).min(ids.len());
            let p = self.window_score(&self.wrap(&ids[start..end])?)?;
            if best.as_ref().is_none_or(|(top, _)| p > *top) {
                best = Some((p, start..end));
            }
        }
        let Some((p, window)) = best else {
            return Ok((0.0, None));
        };
        Ok((p, byte_span(text, encoding.get_offsets(), window)))
    }
}

/// The bytes of `text` that the tokens in `window` came from, given the
/// tokenizer's per-token byte `offsets`, widened to whole characters (a
/// byte-fallback token can split one). `None` for an empty window or
/// offsets that don't fit `text` — the caller then has no window to show.
fn byte_span(text: &str, offsets: &[(usize, usize)], window: Range<usize>) -> Option<Range<usize>> {
    if window.is_empty() || window.end > offsets.len() {
        return None;
    }
    let (mut start, mut end) = (offsets[window.start].0, offsets[window.end - 1].1);
    if start >= end || end > text.len() {
        return None;
    }
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    Some(start..end)
}

/// Where each window of a `len`-token text starts: one window when it fits
/// in `body`, otherwise every `body - overlap` tokens, with the last window
/// pulled back to end exactly at `len` (so it's always full).
pub(crate) fn window_starts(len: usize, body: usize, overlap: usize) -> Vec<usize> {
    if len <= body {
        return vec![0];
    }
    let step = body - overlap;
    let mut starts = Vec::new();
    let mut start = 0;
    loop {
        let clamped = start.min(len - body);
        if starts.last() != Some(&clamped) {
            starts.push(clamped);
        }
        if start + body >= len {
            return starts;
        }
        start += step;
    }
}

fn softmax_at(logits: &[f32], index: usize) -> Option<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f64> = logits.iter().map(|&l| f64::from(l - max).exp()).collect();
    let sum: f64 = exps.iter().sum();
    exps.get(index).map(|e| (e / sum) as f32)
}

/// tract's matrix kernels run on one process-wide executor. Capped at half
/// the cores (at most 4): a scan runs alongside the chat model, which on a
/// CPU-only machine is using the rest.
fn configure_threads() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = (cores / 2).clamp(1, 4);
        tract_linalg::multithread::set_default_executor(
            tract_linalg::multithread::Executor::multithread(threads),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_text_is_one_window() {
        assert_eq!(window_starts(0, 510, 64), vec![0]);
        assert_eq!(window_starts(510, 510, 64), vec![0]);
    }

    #[test]
    fn a_long_text_overlaps_and_ends_on_a_full_window() {
        assert_eq!(window_starts(511, 510, 64), vec![0, 1]);
        assert_eq!(window_starts(1000, 510, 64), vec![0, 446, 490]);
        let starts = window_starts(5000, 510, 64);
        assert_eq!(starts.first(), Some(&0));
        assert_eq!(*starts.last().unwrap() + 510, 5000);
        assert!(starts.windows(2).all(|w| w[1] - w[0] <= 446));
    }

    #[test]
    fn a_window_maps_to_the_bytes_its_tokens_came_from() {
        let text = "héllo wörld again";
        // "héllo" | " wörld" | " again", as byte offsets.
        let offsets = [(0, 6), (6, 13), (13, 19)];
        assert_eq!(byte_span(text, &offsets, 1..3), Some(6..19));
        assert_eq!(
            &text[byte_span(text, &offsets, 0..2).unwrap()],
            "héllo wörld"
        );
        assert_eq!(byte_span(text, &offsets, 1..1), None);
        // An edge inside "é" (bytes 1..3) takes in the whole character.
        assert_eq!(byte_span(text, &[(0, 2)], 0..1), Some(0..3));
        assert_eq!(byte_span(text, &[(0, 6), (2, 6)], 1..2), Some(1..6));
        assert_eq!(byte_span(text, &offsets, 2..4), None);
        assert_eq!(byte_span(text, &[(0, 99)], 0..1), None);
    }

    #[test]
    fn softmax_picks_the_injection_class() {
        let p = softmax_at(&[-2.0, 2.0], 1).unwrap();
        assert!((p - 0.982).abs() < 0.001, "{p}");
        assert!(softmax_at(&[0.0], 1).is_none());
    }
}
