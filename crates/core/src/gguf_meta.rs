//! Parse a GGUF file's binary header, metadata key-value section, and
//! tensor-info table — enough to learn whether a model is a Mixture-of-Experts
//! and its exact (not size-estimated) parameter counts, without ever reading
//! actual tensor weight data. Shared by two callers with two different `Read`
//! sources: [`parse_local`] reads a file straight off disk (`custom::local_spec`,
//! synchronous, cheap), and [`parse_bytes`] parses an in-memory prefix
//! (`huggingface::fetch_gguf_header_meta`'s HTTP range-fetch of a remote repo's
//! file — the metadata+tensor-info section always precedes tensor data, so a
//! bounded prefix is enough, though for models with a large tokenizer vocab
//! array that prefix may need to be generous).
//!
//! ## Format (GGUF v2/v3 — v1 isn't supported: its string lengths are `u32`,
//! not `u64`, and no model worth detecting today ships as v1)
//!
//! ```text
//! magic "GGUF" (4 bytes)
//! version: u32
//! tensor_count: u64
//! metadata_kv_count: u64
//! <metadata_kv_count> KV pairs: { key: string, type: u32, value }
//! <tensor_count> tensor-info entries: { name: string, n_dims: u32,
//!                                       dims: [u64; n_dims], ggml_type: u32,
//!                                       offset: u64 }
//! ```
//! A `string` is `{ len: u64, utf8 bytes }`. A value's `type` tag selects a
//! fixed-size scalar, or (tag 9) an array: `{ elem_type: u32, len: u64,
//! elements... }`, recursively — arrays can hold strings (the tokenizer
//! vocab is exactly this shape), so a fixed-size skip would desync the
//! cursor for every KV after it.
//!
//! MoE detection reads `<architecture>.expert_count` /
//! `<architecture>.expert_used_count` (the `.ends_with` match is
//! architecture-agnostic — Mixtral, Qwen-MoE, DeepSeek, etc. all write these
//! suffixes under their own architecture prefix) and identifies
//! expert-conditioned tensors by name (`blk.N.ffn_{gate,down,up}_exps.weight`,
//! the merged expert-stack shape current llama.cpp conversions produce),
//! cross-checked against the KV's `expert_count` appearing as one of the
//! tensor's own dimensions. Older per-expert-indexed tensors
//! (`blk.N.ffn_gate.0.weight`, `.1.weight`, …) are not recognized — not known
//! to be in current use, flagged here rather than guessed at.

use std::io::Read;
use thiserror::Error;

const GGUF_MAGIC: &[u8; 4] = b"GGUF";

// Sanity ceilings against a corrupt or hostile file — generous enough for any
// real model (a huge multilingual vocab is still under a million tokens; a
// huge MoE model still has at most a few thousand tensors) while guarding
// against a bogus length field driving an enormous allocation or an infinite
// loop before the reader would otherwise hit `Truncated`.
const MAX_KV_COUNT: u64 = 100_000;
const MAX_TENSOR_COUNT: u64 = 200_000;
const MAX_DIMS: u32 = 8;
const MAX_ARRAY_LEN: u64 = 10_000_000;
const MAX_STRING_LEN: u64 = 100_000_000;

mod value_type {
    pub const UINT8: u32 = 0;
    pub const INT8: u32 = 1;
    pub const UINT16: u32 = 2;
    pub const INT16: u32 = 3;
    pub const UINT32: u32 = 4;
    pub const INT32: u32 = 5;
    pub const FLOAT32: u32 = 6;
    pub const BOOL: u32 = 7;
    pub const STRING: u32 = 8;
    pub const ARRAY: u32 = 9;
    pub const UINT64: u32 = 10;
    pub const INT64: u32 = 11;
    pub const FLOAT64: u32 = 12;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GgufMetaError {
    /// Ran out of bytes mid-parse. For a range-fetched prefix, the caller
    /// should retry with a larger range; for a local file, it's corrupt.
    #[error("truncated GGUF data")]
    Truncated,
    /// Bad magic, an unsupported version, an implausible count, or an
    /// unrecognized type tag — never retryable with more bytes.
    #[error("malformed GGUF data: {0}")]
    Malformed(String),
}

/// What this parser extracts from a GGUF file's metadata + tensor info.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GgufMeta {
    pub architecture: Option<String>,
    pub expert_count: Option<u32>,
    pub expert_used_count: Option<u32>,
    /// Sum of every tensor's element count (not byte size) — the model's
    /// exact total parameter count.
    pub total_params: u64,
    /// Sum of element counts for tensors that are part of a MoE expert stack
    /// — the portion only a fraction of which is active per token.
    pub expert_conditioned_params: u64,
}

impl GgufMeta {
    /// `(params_b, moe, active_params_b)` — the shape `ModelSpec` needs.
    /// `moe` requires `expert_count > 1`; `active_params_b` further requires
    /// a known `expert_used_count` and some expert-conditioned weight to
    /// apply that ratio to (never a divide-by-zero: `moe` already implies
    /// `expert_count` is `Some` and non-zero).
    pub fn model_shape(&self) -> (f32, bool, Option<f32>) {
        let params_b = self.total_params as f32 / 1e9;
        let moe = self.expert_count.is_some_and(|n| n > 1);
        let active_params_b = moe
            .then(|| self.expert_used_count.zip(self.expert_count))
            .flatten()
            .filter(|_| self.expert_conditioned_params > 0)
            .map(|(used, total_experts)| {
                let dense = (self.total_params - self.expert_conditioned_params) as f64;
                let active = dense
                    + self.expert_conditioned_params as f64 * used as f64 / total_experts as f64;
                (active / 1e9) as f32
            });
        (params_b, moe, active_params_b)
    }
}

fn read_exact_or_truncated<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<(), GgufMetaError> {
    r.read_exact(buf).map_err(|_| GgufMetaError::Truncated)
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16, GgufMetaError> {
    let mut buf = [0u8; 2];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, GgufMetaError> {
    let mut buf = [0u8; 4];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64, GgufMetaError> {
    let mut buf = [0u8; 8];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_f32<R: Read>(r: &mut R) -> Result<f32, GgufMetaError> {
    let mut buf = [0u8; 4];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(f32::from_le_bytes(buf))
}

fn read_f64<R: Read>(r: &mut R) -> Result<f64, GgufMetaError> {
    let mut buf = [0u8; 8];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(f64::from_le_bytes(buf))
}

fn read_string<R: Read>(r: &mut R) -> Result<String, GgufMetaError> {
    let len = read_u64(r)?;
    if len > MAX_STRING_LEN {
        return Err(GgufMetaError::Malformed(format!(
            "implausible string length {len}"
        )));
    }
    let mut buf = vec![0u8; len as usize];
    read_exact_or_truncated(r, &mut buf)?;
    String::from_utf8(buf)
        .map_err(|_| GgufMetaError::Malformed("string isn't valid utf-8".to_string()))
}

/// A parsed metadata value, scalar or (recursively) array. Only a handful of
/// keys are ever inspected (see [`parse`]); everything else is read purely to
/// keep the cursor in sync with the next KV or tensor-info entry.
#[derive(Debug, Clone)]
enum GgufValue {
    UInt(u64),
    Int(i64),
    Str(String),
    Other,
}

impl GgufValue {
    fn as_u64(&self) -> Option<u64> {
        match *self {
            GgufValue::UInt(v) => Some(v),
            GgufValue::Int(v) if v >= 0 => Some(v as u64),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            GgufValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn read_value<R: Read>(r: &mut R, type_tag: u32) -> Result<GgufValue, GgufMetaError> {
    use value_type::*;
    Ok(match type_tag {
        UINT8 => GgufValue::UInt(read_exact_byte(r)? as u64),
        INT8 => GgufValue::Int(read_exact_byte(r)? as i8 as i64),
        UINT16 => GgufValue::UInt(read_u16(r)? as u64),
        INT16 => GgufValue::Int(read_u16(r)? as i16 as i64),
        UINT32 => GgufValue::UInt(read_u32(r)? as u64),
        INT32 => GgufValue::Int(read_u32(r)? as i32 as i64),
        FLOAT32 => {
            read_f32(r)?;
            GgufValue::Other
        }
        BOOL => {
            read_exact_byte(r)?;
            GgufValue::Other
        }
        STRING => GgufValue::Str(read_string(r)?),
        UINT64 => GgufValue::UInt(read_u64(r)?),
        INT64 => GgufValue::Int(read_u64(r)? as i64),
        FLOAT64 => {
            read_f64(r)?;
            GgufValue::Other
        }
        ARRAY => {
            let elem_type = read_u32(r)?;
            let len = read_u64(r)?;
            if len > MAX_ARRAY_LEN {
                return Err(GgufMetaError::Malformed(format!(
                    "implausible array length {len}"
                )));
            }
            for _ in 0..len {
                read_value(r, elem_type)?;
            }
            GgufValue::Other
        }
        other => {
            return Err(GgufMetaError::Malformed(format!(
                "unknown value type tag {other}"
            )))
        }
    })
}

fn read_exact_byte<R: Read>(r: &mut R) -> Result<u8, GgufMetaError> {
    let mut buf = [0u8; 1];
    read_exact_or_truncated(r, &mut buf)?;
    Ok(buf[0])
}

fn checked_element_count(dims: &[u64]) -> Result<u64, GgufMetaError> {
    dims.iter()
        .try_fold(1u64, |acc, &d| acc.checked_mul(d))
        .ok_or_else(|| GgufMetaError::Malformed("tensor element count overflowed".to_string()))
}

fn parse<R: Read>(r: &mut R) -> Result<GgufMeta, GgufMetaError> {
    let mut magic = [0u8; 4];
    read_exact_or_truncated(r, &mut magic)?;
    if &magic != GGUF_MAGIC {
        return Err(GgufMetaError::Malformed(
            "not a GGUF file (bad magic)".to_string(),
        ));
    }
    let version = read_u32(r)?;
    if version < 2 {
        return Err(GgufMetaError::Malformed(format!(
            "unsupported GGUF version {version}"
        )));
    }
    let tensor_count = read_u64(r)?;
    let kv_count = read_u64(r)?;
    if tensor_count > MAX_TENSOR_COUNT || kv_count > MAX_KV_COUNT {
        return Err(GgufMetaError::Malformed(
            "implausible tensor/kv count".to_string(),
        ));
    }

    let mut architecture = None;
    let mut expert_count = None;
    let mut expert_used_count = None;
    for _ in 0..kv_count {
        let key = read_string(r)?;
        let type_tag = read_u32(r)?;
        let value = read_value(r, type_tag)?;
        if key == "general.architecture" {
            architecture = value.as_str().map(str::to_string);
        } else if key.ends_with(".expert_count") {
            expert_count = value.as_u64().map(|v| v as u32);
        } else if key.ends_with(".expert_used_count") {
            expert_used_count = value.as_u64().map(|v| v as u32);
        }
    }

    let mut total_params: u64 = 0;
    let mut expert_conditioned_params: u64 = 0;
    for _ in 0..tensor_count {
        let name = read_string(r)?;
        let n_dims = read_u32(r)?;
        if n_dims > MAX_DIMS {
            return Err(GgufMetaError::Malformed(
                "implausible tensor rank".to_string(),
            ));
        }
        let mut dims = Vec::with_capacity(n_dims as usize);
        for _ in 0..n_dims {
            dims.push(read_u64(r)?);
        }
        let _ggml_type = read_u32(r)?;
        let _offset = read_u64(r)?;

        let elements = checked_element_count(&dims)?;
        total_params = total_params.checked_add(elements).ok_or_else(|| {
            GgufMetaError::Malformed("total parameter count overflowed".to_string())
        })?;

        let is_expert_stack = (name.contains("ffn_gate_exps")
            || name.contains("ffn_down_exps")
            || name.contains("ffn_up_exps"))
            && expert_count.is_some_and(|ec| dims.contains(&(ec as u64)));
        if is_expert_stack {
            expert_conditioned_params = expert_conditioned_params
                .checked_add(elements)
                .ok_or_else(|| {
                    GgufMetaError::Malformed(
                        "expert-conditioned parameter count overflowed".to_string(),
                    )
                })?;
        }
    }

    Ok(GgufMeta {
        architecture,
        expert_count,
        expert_used_count,
        total_params,
        expert_conditioned_params,
    })
}

/// Parse a GGUF file straight off disk. Reads only up to the end of the
/// tensor-info table — never the tensor weight data that follows it.
pub fn parse_local(path: &std::path::Path) -> Result<GgufMeta, GgufMetaError> {
    let file = std::fs::File::open(path)
        .map_err(|e| GgufMetaError::Malformed(format!("couldn't open file: {e}")))?;
    parse(&mut std::io::BufReader::new(file))
}

/// Parse a (possibly truncated) in-memory prefix of a remote GGUF file — the
/// shape an HTTP range-fetch produces. Returns [`GgufMetaError::Truncated`]
/// when `data` ends mid-parse, telling the caller to fetch more and retry.
pub fn parse_bytes(data: &[u8]) -> Result<GgufMeta, GgufMetaError> {
    parse(&mut std::io::Cursor::new(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }

    fn push_kv_string(buf: &mut Vec<u8>, key: &str, value: &str) {
        push_string(buf, key);
        buf.extend_from_slice(&value_type::STRING.to_le_bytes());
        push_string(buf, value);
    }

    fn push_kv_u32(buf: &mut Vec<u8>, key: &str, value: u32) {
        push_string(buf, key);
        buf.extend_from_slice(&value_type::UINT32.to_le_bytes());
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn push_tensor(buf: &mut Vec<u8>, name: &str, dims: &[u64]) {
        push_string(buf, name);
        buf.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in dims {
            buf.extend_from_slice(&d.to_le_bytes());
        }
        buf.extend_from_slice(&0u32.to_le_bytes()); // ggml_type, unused by this parser
        buf.extend_from_slice(&0u64.to_le_bytes()); // tensor data offset, unused
    }

    fn header(tensor_count: u64, kv_count: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(GGUF_MAGIC);
        buf.extend_from_slice(&3u32.to_le_bytes()); // version 3
        buf.extend_from_slice(&tensor_count.to_le_bytes());
        buf.extend_from_slice(&kv_count.to_le_bytes());
        buf
    }

    #[test]
    fn parses_a_dense_model() {
        let mut buf = header(1, 1);
        push_kv_string(&mut buf, "general.architecture", "llama");
        push_tensor(&mut buf, "token_embd.weight", &[4096, 32000]);

        let meta = parse_bytes(&buf).expect("should parse");
        assert_eq!(meta.architecture.as_deref(), Some("llama"));
        assert_eq!(meta.total_params, 4096 * 32000);
        assert_eq!(meta.expert_conditioned_params, 0);

        let (params_b, moe, active_params_b) = meta.model_shape();
        assert!(!moe);
        assert_eq!(active_params_b, None);
        assert!(params_b > 0.0);
    }

    #[test]
    fn parses_an_moe_model_and_computes_active_params() {
        let mut buf = header(2, 3);
        push_kv_string(&mut buf, "general.architecture", "qwen3moe");
        push_kv_u32(&mut buf, "qwen3moe.expert_count", 8);
        push_kv_u32(&mut buf, "qwen3moe.expert_used_count", 2);
        push_tensor(&mut buf, "token_embd.weight", &[100, 100]); // 10_000 dense params
        push_tensor(&mut buf, "blk.0.ffn_gate_exps.weight", &[8, 50, 50]); // 20_000 expert params

        let meta = parse_bytes(&buf).expect("should parse");
        assert_eq!(meta.expert_count, Some(8));
        assert_eq!(meta.expert_used_count, Some(2));
        assert_eq!(meta.total_params, 10_000 + 20_000);
        assert_eq!(meta.expert_conditioned_params, 20_000);

        let (params_b, moe, active_params_b) = meta.model_shape();
        assert!(moe);
        // dense(10_000) + expert(20_000) * used(2)/total(8) = 15_000
        let expected_active_b = 15_000f32 / 1e9;
        assert!((active_params_b.unwrap() - expected_active_b).abs() < 1e-9);
        assert!(params_b > 0.0);
    }

    #[test]
    fn rejects_bad_magic() {
        let buf = b"NOPE".to_vec();
        assert!(matches!(
            parse_bytes(&buf),
            Err(GgufMetaError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_version_1() {
        let mut buf = Vec::new();
        buf.extend_from_slice(GGUF_MAGIC);
        buf.extend_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            parse_bytes(&buf),
            Err(GgufMetaError::Malformed(_))
        ));
    }

    #[test]
    fn truncated_input_is_retryable() {
        let mut buf = header(1, 1);
        push_kv_string(&mut buf, "general.architecture", "llama");
        buf.truncate(buf.len() - 2); // cut off mid-tensor-info
        assert_eq!(parse_bytes(&buf), Err(GgufMetaError::Truncated));
    }

    #[test]
    fn skips_a_string_array_without_desyncing() {
        // A tokenizer-vocab-shaped array KV must not desync the cursor — the
        // architecture KV and tensor after it must still parse correctly.
        let mut buf = header(1, 2);
        push_string(&mut buf, "tokenizer.ggml.tokens");
        buf.extend_from_slice(&value_type::ARRAY.to_le_bytes());
        buf.extend_from_slice(&value_type::STRING.to_le_bytes());
        buf.extend_from_slice(&3u64.to_le_bytes());
        push_string(&mut buf, "a");
        push_string(&mut buf, "bb");
        push_string(&mut buf, "ccc");
        push_kv_string(&mut buf, "general.architecture", "llama");
        push_tensor(&mut buf, "token_embd.weight", &[10, 10]);

        let meta = parse_bytes(&buf).expect("should parse past the array");
        assert_eq!(meta.architecture.as_deref(), Some("llama"));
        assert_eq!(meta.total_params, 100);
    }

    #[test]
    fn moe_without_a_known_used_count_has_no_active_estimate() {
        let mut buf = header(1, 1);
        push_kv_u32(&mut buf, "mixtral.expert_count", 8);
        push_tensor(&mut buf, "blk.0.ffn_gate_exps.weight", &[8, 50, 50]);

        let meta = parse_bytes(&buf).expect("should parse");
        let (_, moe, active_params_b) = meta.model_shape();
        assert!(moe, "expert_count > 1 alone is enough to call it MoE");
        assert_eq!(
            active_params_b, None,
            "no expert_used_count means no active-param estimate"
        );
    }
}
