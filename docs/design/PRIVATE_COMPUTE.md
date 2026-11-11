# Private compute — can the inference host share compute without reading requests or replies?

**Spike finding for [issue #44](https://github.com/myopenpal-ai/openpal/issues/44)**
("spike(remote-access): can the inference host share compute without reading
requests or replies"), answering the question for the remote-inference epic
[#41](https://github.com/myopenpal-ai/openpal/issues/41). Finding only — no
code was written and this ticket deliberately ships none.

- **Date:** 2026-11-11
- **Verdict:** **No-go (Outcome B) today.** A realistic "private compute" mode
  does not exist for OpenPal's hardware, and the two technique families that
  could provide it are either unavailable on Apple Silicon or 3–6 orders of
  magnitude too slow. The epic should proceed with transport-encrypted-only
  remote inference; this document records why, so the question is not
  re-litigated without a *changed* fact (revisit conditions at the end).
- **Property being assessed:** a mode where the machine running the model
  ("the server") **cannot** read the client's request **or** the generated
  reply — it only shares its computational power. "Cannot" is read strictly:
  even a fully privileged, actively malicious host (root, kernel, physical
  access) cannot recover the text. Anything weaker — "no ordinary software
  path", "we promise not to look", "requests aren't persisted" — is a
  different, trust-based property and is addressed as such below.

---

## 1. The escalation this spike makes explicit

`docs/REMOTE_INFERENCE.md`'s current threat model is: *transport-encrypted
channel (TLS with TOFU pinning + per-device bearer token) between two devices
the same user set up on an already-secure LAN or overlay network*. The
adversary in that model is a **network** actor (and, secondarily, a stolen
token). The **host itself is never an adversary**: it decrypts the request,
runs the model on it, sees every intermediate activation and the final reply
in memory, and is merely *trusted not to* persist or exfiltrate them.

"Private compute" escalates to **host-as-adversary**: the property that the
host cannot read the text even if it wants to. That is a categorically
stronger guarantee, and the whole point of this document is that the
escalation is where the wall is. It is also worth saying plainly when the
property is *needed*: in OpenPal's stated use case both ends belong to the
same person, so the host is the user's own desktop and there is usually no
adversary at all. The property only starts to matter if (a) the user runs the
host on a machine they don't fully control (shared, managed by an employer),
(b) the host is suspected compromised, or (c) the product widens into serving
inference to *third parties* (the "share your idle Mac's compute" scenario).
Only (c) is a natural fit for the strict property, and a real, funded project
exists for exactly that (candidate C5, §7) — its conclusions on Apple hardware
matter for us.

Two more precise disclaimers, so nobody over-reads any candidate below:

- **Text-blind does not mean metadata-blind.** Every scheme we found leaks
  metadata: request/reply length (ciphertext size, token count), generation
  timing, KV-state growth, model and sampling parameters. Themis, the FHE
  result we rely on most, states this explicitly ([R3]: sequence length,
  runtime and sampling parameters are "public information"). The spike's
  target property is *text* confidentiality; nobody gets absolute blindness.
- **Weights are not in scope here.** In OpenPal's model the weights already
  live on the host (it is the user's own machine), so "private compute" is
  about *user data*, not model IP. The candidates and the cost table in §8
  mention weights only where they affect the assessed property; TEE
  deployments protect weights too, but that is not the question this ticket
  asked.

---

## 2. Why the cheap-sounding answers are structurally impossible

Before surveying the real candidates, it is worth killing the family of
schemes that *sound* like "share compute without reading": split/blinded
inference, "client keeps a layer", "client keeps the embeddings", "scramble
the tokens". None of them can work for autoregressive decoder-only inference,
and the reason is now a *theorem*, not folklore.

**Decoder-only transformer hidden states provably contain the input.**
Crisostomi et al. prove that for a standard causal decoder-only transformer,
the map from prompt sequences to (last-token, and per-layer) hidden
representations is **almost-surely injective** — different prompts map to
different hidden states for essentially all parameter settings, at
initialization and throughout training — and give **SipIt**, an algorithm that
reconstructs the exact input from hidden states in provable linear time
([R11]). In plain words: *if the host computes on (or can observe) the hidden
states of your model, it holds the information that uniquely determines your
text, and only needs the weights — which it has — to read it.*

The practical attacks confirm the theorem, exactly on OpenPal-class models:

- Gradient-based inversion of GPT-2 last-layer hidden states reaches ~97.5%
  exact-match token recovery (mean similarity 0.994); the authors conclude
  last-layer hidden states are "as sensitive as the original text" ([R12]).
- **Even the next-token probabilities leak the prompt**: language-model
  inversion from the model's own output distribution reconstructs Llama-2-7B
  prompts with BLEU 59 / token-F1 78, 27% recovered exactly ([R13]). This is
  decisive for the *reply* side too: sampling happens on the host, so the host
  necessarily materializes per-token logits over the whole vocabulary for
  every generated token. Any "blind" scheme that runs the network in the
  clear leaks the reply through the very act of generating it.
- Input reconstruction from hidden states at arbitrary depth: Embed Parrot on
  ChatGLM-6B and Llama-2-7B ([R14]); "Depth Gives a False Sense of Privacy"
  inverts Llama-3 middle-layer states with 86.88 token-F1 on a 4,112-token
  medical prompt ([R15]); split-learning LLM inversion reaches 38–75% attack
  scores on the split boundary ([R16]).

Two consequences:

1. **Any split where the host runs "most of the model" in the clear is
   broken**, no matter how thin the client-held slice. The host sees enough
   to read.
2. **The reply is strictly harder to hide than the request.** The request can
   at least be *encrypted before arrival*; the reply is *generated* on the
   host. Every scheme that actually works must keep **every** intermediate
   value (hidden states, logits, sampling decision) opaque to the host for
   both directions. That constraint leaves exactly two families: run the whole
   computation inside a hardware enclave the host cannot inspect (TEE), or run
   it under cryptography that keeps every value encrypted/shared (FHE/MPC).
   Those are the only candidates below.

An architectural note that rules nothing out but shapes everything: OpenPal's
gateway is a *raw-byte proxy* to llama-server's `/v1/chat/completions`
(`docs/REMOTE_INFERENCE.md`), and the engine is a managed `llama-server`
subprocess speaking plaintext OpenAI-compatible HTTP on loopback
(`ARCHITECTURE.md`). Every candidate below that changes *what* the engine
computes changes this seam as well — none is a drop-in proxy tweak.

---

## 3. Candidate C1 — Hardware trusted execution / confidential computing (the industry's answer)

**Mechanism.** Encrypted memory + hardware isolation for the whole inference
stack, plus remote attestation so a client can verify what it is about to send
to. In production form this is a CPU TEE (Intel TDX, AMD SEV-SNP) attached to
a GPU TEE (NVIDIA Hopper/Blackwell "confidential computing" mode), with the
client verifying both via an attestation service (e.g. Azure Attestation) and
releasing keys only to attested environments ([R17][R18][R19]). The host OS,
hypervisor and operators are outside the trust boundary; the hardware vendors
(and their firmware) are inside it.

**Cost.** This is the *only* candidate whose cost is acceptable: independent
measurements find <9% throughput overhead on H100/H200 for LLM inference
(typically ~4–8%; TTFT up to ~19–26% on small models, shrinking to ~0% on
70B and longer sequences) ([R20]); the ETH SPCL study finds 4–8% GPU-CPU TEE
throughput penalties and <10% CPU-TEE overhead with AMX ([R21]). Caveats
exist — some research measures up to 53–88% throughput drop on specific
configurations ([R22][R23]) — but the order of magnitude is "near plaintext",
which no cryptographic candidate approaches.

**Why it is still a no-go for OpenPal.** Three reasons, any one decisive:

1. **The hardware does not exist on OpenPal's platforms.** Confidential GPUs
   are enterprise data-center parts (H100/H200/Blackwell/Rubin, Azure NCc
   confidential VMs). Apple Silicon — the only shipping target — **exposes no
   TEE to third-party code**: the Secure Enclave guards keys and produces
   signatures/attestation, but does not run the model or encrypt general
   memory ([R9][R10]). Apple's own confidential inference (Private Cloud
   Compute) runs inside Apple-operated data centers on AMD SEV-SNP + NVIDIA CC
   hardware with Apple as attestation root ([R24][R25]) — not something a
   local app on a consumer Mac can use.
2. **Attestation is an operated service.** A client verifying "this host is a
   genuine TEE running unmodified OpenPal" needs a verifier that is
   trust-anchored somewhere. Whoever runs it operates backend infrastructure —
   precisely what `ARCHITECTURE.md`'s "local-first, no operated backend" and
   this ticket's constraints forbid.
3. **The engine contract would have to move inside the enclave.** Today's
   `llama-server` subprocess model would need the *in-process* engine
   (`docs/design/MOBILE.md`'s planned `InProcessBackend`) to run inside the
   enclave, because any subprocess/loopback hop outside the enclave is
   readable. That's a new engine backend plus a TEE bring-up on a platform
   that has no TEE.

**Threat-model coverage.** Text: unreadable to the host OS/operator (up to
the hardware TCB). Reply: unreadable (sampling inside the enclave). Metadata:
still visible. Side channels: residual, and historically real for every TEE
class; not addressed here but never zero. The honest summary of TEE is: it is
the industry's answer (Azure confidential GPU VMs, NVIDIA, Google's
confidential AI, Apple PCC, Cloudflare's private Workers AI) because it is the
only thing cheap enough — and it is still wrong for OpenPal because of the
hardware and the operated-attestation requirements.

---

## 4. Candidate C2 — Fully homomorphic encryption (the only single-host, weak-client "cannot")

**Mechanism.** The client encrypts the prompt with an FHE scheme (CKKS);
the server evaluates the whole model on ciphertexts and returns encrypted
logits; only the client can decrypt. Crucially, this is **non-interactive**:
the client does not need to be a strong compute party — it encrypts once and
decrypts once, which a phone can do. It is the *only* cryptographic family
whose topology matches OpenPal's (one host, one weak client) for the *request*
direction. The reply requires per-token rounds (client decrypts logits and
samples, or the server samples homomorphically via a blind argmax — both
exist in the literature, e.g. NEXUS's encrypted argmax [R2]); the per-token
model forward is server-side FHE regardless.

**Cost.** The numbers below are the *best open results as of late 2026*, all
on data-center GPUs (an H100 exceeds an M-series Mac's FLOPs by over an order
of magnitude, and these stacks are CUDA-only — running them on a Mac is not a
configuration anyone has built):

- **Themis** (Oct 2026; first open-source end-to-end CKKS Llama-3-8B on one
  H100): **366.4 s server-side for a single 128-token forward pass**, 58.9 GiB
  peak device memory, vs. 1651.9 s for the prior THOR baseline ([R3]). That is
  ~7000× the plaintext prefill on the same hardware, and per-token decode —
  one forward per generated token — is the remaining wall: the newest work in
  the field headlines **a single encrypted token decode** as its result
  ([R26]).
- **Small-model results**: BERT-base in ~37 s on a GPU with 164 MB bandwidth
  (NEXUS [R2]); ~7 s amortized per input on a 32 GB RTX 5090 (Terrazzo [R7]).
  Even these are 3–4 orders of magnitude over plaintext.
- **Cross-check**: GPU-accelerated FHE GPT-2 forward passes are ~200× faster
  than CPU FHE ([R27]) — i.e. the CPU-FHE baseline (what a Mac would run
  without a CUDA stack) is hours-per-answer territory for OpenPal-sized models.
- Model quality survives: polynomial/quantized approximations keep
  perplexity essentially at plaintext (e.g. 7.77 vs 7.76 perplexity in [R26];
  PUMA-style MPC approximations report matching accuracy [R5]) — *quality is
  not the blocker; speed is*.

**Why it is still a no-go for OpenPal.** The cost is the whole story: 3–6
orders of magnitude latency means a single 200-token answer takes **hours on
the only hardware class that exists for this today**, and minutes per token
even in the best 2026 results. It also requires a *different inference
runtime* (each system re-packs weights and re-tunes polynomial approximations
per model; see e.g. Themis's packing scheme [R3]) — nothing near llama.cpp or
the GGUF contract the catalog, bundler and engine all assume, and nothing
runs on Metal. In OpenPal terms: a beginner turning on remote access would
wait longer for one reply than they waited to download the model.

---

## 5. Candidate C3 — Secure multi-party computation / secret sharing

**Mechanism.** Client and server (typically **two non-colluding servers**)
hold additive secret shares of the prompt; every nonlinearity (softmax, GELU,
layernorm) and every secret×secret product requires interaction — Beaver
triples, FSS, OT. This is the fastest *cryptographic* family on LAN-class
links, and the interactive structure is why the attack surface is small, but
also why it needs strong parties.

**Cost.** Representative end-to-end numbers (all on data-center hardware,
LAN links, FP16 weights):

- **SPEC** (USENIX 2026): Llama-3.1-8B on 2× A100 non-colluding servers,
  TTFT **1.33 s at n=8** and **9.78 s at n=128**, with 0.22–4.66 GB online
  communication ([R4]).
- **PUMA**: Llama-7B, **~200 s per generated token**, 3× 128-vCPU cloud
  servers, 20 Gbps, 1.8 GB communication ([R5]).
- Earlier systems on the same table: BumbleBee Llama-7B TTFT ~832 s at n=8;
  NEXUS ~52 s; Sigma Llama-2-7B ~23 s at n=128 ([R4][R2]).

**Why it is still a no-go for OpenPal.** Two structural blockers, not just
cost:

1. **It needs a second, non-colluding party** (or a client strong enough to be
   an active protocol party). OpenPal has one host plus a phone client; a
   phone cannot meaningfully be an MPC party, and standing up a second
   non-colluding server is an operated backend, which `ARCHITECTURE.md` and
   this ticket forbid.
2. **The models must be modified.** Each system replaces GELU/softmax with
   MPC-friendly approximations, typically re-trained/distilled to recover
   quality (MPCformer: 2.2× faster at 97% of original quality via
   approximation + knowledge distillation [R1]). That is a different model
   artifact for the same "mono-model" — effectively a new catalog of
   MPC-tuned weights per model, per quant level, per context length.

MPC is the right answer for *cloud* settings with two datacenter parties and
a compliance budget; it is the wrong shape for a local-first desktop product.

---

## 6. Candidate C4 — Split inference / "blinding" (not a "cannot", shown for completeness)

Because it is the most volunteered idea ("client keeps a layer, server does
the rest"), it earns a row: it is **not a candidate for the assessed
property**. The host still runs most of the network in the clear and observes
hidden states at the split boundary; §2's injectivity theorem and the
inversion attacks make that readable (SipIt needs only "the model up to layer
ℓ" [R11]; shallow-layer inversion is *easier* than deep [R14]) — and the
Reply side is worse, because logits must be materialized host-side to sample
([R13]). The honest statement is: split inference trades "host can read
everything" for "host can read everything but it's slightly more work". It
adds engineering cost without adding the property. The same verdict applies
to token-scrambling / embedding-hiding variants: the host holds the model,
and embeddings are only a thin, invertible prefix of what it computes.

## 7. Candidate C5 — "No ordinary software path" hardening (the only thing anyone has shipped on Apple Silicon)

There is a real, funded project that asks *exactly* this ticket's question
for *exactly* OpenPal's hardware: Eigen Labs' **Darkbloom** / d-inference
"private inference on idle Macs" (2026): *"a user wants to run an AI model on
someone else's Mac, but the machine's owner — who has root access and
physical custody — should not be able to see the user's prompts or the
model's responses. The machines lack hardware TEEs."* ([R9], [R10]).

Their answer is worth quoting as a boundary: they eliminate "every software
path" (in-process engine, no subprocess/loopback, `PT_DENY_ATTACH`, hardened
runtime, SIP immutability argument), then bind identity through a Secure
Enclave signature + Apple-signed certificate chain + MDM/attestation + app-code
identity attestation via APNs — and still concede the residual threat is
**physical memory probing** and, per independent critique, that there is **no
hardware enclave** (Apple offers no `DCAppAttest` on macOS), that binary
hashes are self-reported, and that the system is unaudited ([R9][R10]).

Two takeaways for OpenPal:

1. **Even a purpose-built, funded system cannot reach "cannot" on Apple
   Silicon** — the best available is "no ordinary software path" + a
   trust/attestation chain, i.e. a *stronger* version of "trusted not to",
   not a provable one. This is the ceiling for the assessed property on our
   hardware.
2. What it took to get even there is a **coordinator** (routing, attestation
   verification, key management, billing) — an operated backend of exactly
   the kind OpenPal refuses to run, and a product pivot (serving third
   parties) far beyond the remote-inference epic.

---

## 8. Cost summary (orders of magnitude, ~7–13B Q4-class model)

Baseline for comparison: plaintext on an M-series Mac (OpenPal's actual
hardware), llama.cpp Q4, RAM-resident — on the order of **10–50 tokens/s**
(the repo's own measurements: gemma-4-12b ≈ 16 tok/s on a base M4;
disk-slow MoE models as low as ~0.1–1.3 tok/s — `docs/design/SPEED_ESTIMATE_ACCURACY.md`).

| Candidate | Compute vs plaintext | Latency class | Memory / comm | Model format | Weak (phone) client OK? | Provides "cannot"? |
| --- | --- | --- | --- | --- | --- | --- |
| **C1 TEE/CC** | ~1× (4–20% typical) | normal | near-plaintext | unchanged (llama.cpp could run in-enclave) | yes (verifies attestation) | yes, up to hardware TCB — **but no Apple hardware, needs operated attestation** |
| **C2 FHE** | ~10³–10⁶× | 366 s per 128-token forward of 8B on an H100; hours on a Mac | ~4× weights (58.9 GiB peak for 8B) | custom runtime + per-model packing/approximations (no GGUF) | yes (encrypt/decrypt only) | yes (RLWE) — **cost kills it** |
| **C3 MPC** | ~10²–10⁴× | 1.3 s TTFT (8B, n=8) to ~200 s/token (7B) on datacenter | GBs of online comm per short prompt | modified (GELU/softmax approximated + re-trained) | no (needs 2nd non-colluding server) | yes (against either server) — **needs 2nd party** |
| **C4 split/blinding** | ~1× | normal | small | unchanged | yes | **no** (invertible) |

All cryptographic candidates also leak metadata (lengths, timing, token
counts) and none helps with side channels; TEE leaks the same metadata and has
residual side-channel and TCB-vendor trust.

---

## 9. Fit with OpenPal's constraints

- **Beginner-first.** C1 is invisible to the beginner (fine) but unavailable;
  C2/C3 turn every reply into a minutes-to-hours wait and C3 needs a second
  machine — all three break the "it just answers" experience. The only
  beginner-compatible "privacy" change available now is ephemerality (below).
- **Mono-model first.** The assessed property is *not* a second-model
  question — but C2 and C3 silently are: each requires its own runtime and a
  per-model, per-quantity approximation artifact, i.e. a parallel, differently
  tuned copy of every supported model. That multiplies the catalog/disk/RAM
  problem the mono-model rule exists to avoid, and no copy exists for OpenPal's
  curated GGUF catalog anyway.
- **Local-first, no operated backend.** C1 needs an attestation verifier
  (operated service). C3 needs a second non-colluding server (operated or
  the user's own second machine — unrealistic). C2 is the only one that stays
  single-host and unoperated — and it is the slowest. C5 needs a coordinator.
  The ticket explicitly asked this spike to flag any candidate that requires
  an operated backend; all of C1, C3 and C5 do.
- **Engine/gateway contract.** C1 additionally requires moving inference
  into an in-enclave process (the long-planned `InProcessBackend`); C2/C3
  replace llama-server with custom crypto runtimes. The current gateway's
  raw-byte proxy does not survive any of these — every "cannot" mode is a
  protocol change, not a proxy tweak.

### The honest step available today: ephemerality, advertised as such

If the epic wants the *value* the spike question gestures at — a remote
client's data not lingering on the host — without pretending to blindness,
it can ship (and document) **ephemerality**: remote-request plaintext never
persisted (no conversation-store write for proxied traffic — true today, the
gateway is a passthrough), no request/reply logging from the gateway (extend
the existing `logging::redact` discipline to the remote path), and memory
hygiene in the proxy. This is cheap, beginner-invisible, consistent with
`crates/core/src/crypto/` conventions (no new key material needed), and it
must be marketed precisely: *"the host never stores or logs your chat"* is
true and valuable; *"the host cannot read your chat"* is false on today's
hardware and must not be claimed.

### Consistency with existing primitives (if this is ever revisited)

Staying consistent with `crates/core/src/crypto/` costs nothing to record
here: a hypothetical long-term FHE mode would use the existing
`secret_store`/key-wrapping posture to hold a per-pairing inference key,
derive per-device subkeys via `derive_subkey` (the `kdf.rs` convention), and
reuse the pairing/TLS machinery to deliver encrypted outputs — i.e. the
crypto plumbing is already in the shape this would need; it is the *engine*
and the *economics* that are missing, not the key handling.

---

## 10. Go / no-go recommendation

**Recommendation: no-go on a "private compute" mode for the remote-inference
epic (Outcome B).** Proceed with transport-encrypted-only remote inference.
Record three revisit conditions — a change in *any* of them re-opens the
question and this document should be re-read then:

1. **Apple (or a vendor whose parts OpenPal ships on) exposes a third-party
   usable TEE with attestation on consumer/Apple hardware.** Nothing on the
   public roadmap suggests this; it is the only event that makes C1 fit.
2. **An open-source FHE transformer stack reaches ~10× plaintext on consumer
   GPUs and integrates with the GGUF/llama.cpp engine contract.** Current
   state: 10³–10⁶× on data-center GPUs, standalone runtimes, no llama.cpp
   integration, per-model approximations ([R3][R7][R26]).
3. **Product intent widens to serving third-party users on a user's machine**
   — the Darkbloom scenario. That is a different product (and product
   company) and would itself require an operated coordinator, a separate
   architectural decision.

**What the epic should do instead:** keep transport-encrypted remote
inference; ship the ephemerality commitment above as the honest privacy
measure; and link this finding from the epic so the "can the host read it?"
question has a written answer with citations instead of an assumption.

---

## References

- [R1] *Cryptography-based privacy-preserving LLMs: lifecycle survey* (Springer, AI Review 2025) — MPCformer-style approximation+KD results; FHE/MPC cost drivers (matrix mults, nonlinears). https://link.springer.com/article/10.1007/s10462-025-11466-6
- [R2] **NEXUS** — *Secure Transformer Inference Made Non-interactive* (ePrint 2024/136): BERT-base 37.3 s GPU, 164 MB bandwidth, secure argmax. https://eprint.iacr.org/2024/136.pdf
- [R3] **Themis** — *An Open-Source End-to-End FHE Implementation for Privacy-Preserving Llama-3-8B Inference* (arXiv:2609.12378, Oct 2026): 366.4 s / 58.9 GiB for a 128-token forward on one H100; 4.51× over THOR. https://arxiv.org/abs/2609.12378
- [R4] **SPEC** — *Secure Private Embedding Computation for Real-Time Transformer Inference* (USENIX): Llama-3.1-8B TTFT 1.33 s (n=8) / 9.78 s (n=128) on 2× A100, 0.22–4.66 GB comm; includes BumbleBee/NEXUS/Sigma comparison table. https://seyfal.com/assets/documents/SPEC_USENIX.pdf
- [R5] **PUMA** — *Secure inference of LLaMA-7B in five minutes* (Security & Safety, 2025): ~200 s/token, 3×128-vCPU, 1.8 GB comm. https://sands.edpsciences.org/articles/sands/full_html/2025/01/sands20250020/sands20250020.html
- [R6] *Privacy-Preserving LLM Inference in Practice: Comparative Survey* (ePrint 2026/105) — operational notion "only the client can read prompt and completion"; deployment trajectory TEE → crypto-augmented → FHE. https://eprint.iacr.org/2026/105.pdf
- [R7] **Terrazzo** — *Memory-Aware GPU Framework for Private Transformers* (ePrint 2026/1870): BERT-base 7.04 s/input on RTX 5090, 29.98 s on A100. https://eprint.iacr.org/2026/1870.pdf
- [R8] *Confidential Computing on NVIDIA Hopper GPUs: Performance Benchmark* (arXiv:2409.03992): <9% throughput overhead LLM serving, TTFT +19–26% small models. https://arxiv.org/abs/2409.03992
- [R9] **Darkbloom / d-inference** — Eigen Labs: *Private Distributed Inference on Consumer Hardware* (2026): Apple Silicon has no third-party TEE; hardening + Secure-Enclave/APNs attestation; residual physical-memory threat. https://raw.githubusercontent.com/Layr-Labs/d-inference/master/papers/dginf-private-inference.pdf
- [R10] *Darkbloom: Private AI Inference on Idle Macs* (imiel.dev, 2026) — technical critique: no hardware enclave on macOS, no DCAppAttest on macOS, self-reported binary hashes, unaudited. https://imiel.dev/blog/darkbloom-private-inference-apple-silicon
- [R11] **SipIt** — *Language Models are Injective and Hence Invertible* (ICLR 2026, arXiv:2510.15511): injectivity theorem for decoder-only transformers; linear-time exact input recovery from hidden states. https://arxiv.org/abs/2510.15511
- [R12] *Recovering Input Text from Hidden States* (arXiv:2607.00852, 2026): gradient inversion of GPT-2 hidden states, 97.5% exact-match. https://arxiv.org/abs/2607.00852
- [R13] *Language Model Inversion* (Morris et al., ICLR 2024): Llama-2-7B prompt reconstruction from next-token probabilities, BLEU 59 / F1 78 / 27% exact. https://proceedings.iclr.cc/paper_files/paper/2024/file/999fcab97007ebef0cda9949550b4a9e-Paper-Conference.pdf
- [R14] **Embed Parrot** — *Information Leakage from Embedding in LLMs* (arXiv:2405.11916): input reconstruction from ChatGLM-6B / Llama-2-7B hidden states at all depths. https://arxiv.org/abs/2405.11916
- [R15] *Depth Gives a False Sense of Privacy: LLM Internal States Inversion* (arXiv:2507.16372): 86.88 token-F1 from Llama-3 middle layers. https://www.alphaxiv.org/abs/2507.16372
- [R16] *Model Inversion in Split Learning for Personalized LLMs* (arXiv:2501.05965): 38–75% attack scores at the split boundary. https://arxiv.org/abs/2501.05965
- [R17] Microsoft: *Unlocking the Potential of Privacy-Preserving AI with Azure Confidential Computing* (2023) — H100 CC + SEV-SNP/TDX, double attestation, SKR. https://techcommunity.microsoft.com/blog/azureconfidentialcomputingblog/unlocking-the-potential-of-privacy-preserving-ai-with-azure-confidential-computi/3776838
- [R18] NVIDIA Confidential Computing (Hopper/Blackwell/Rubin + device attestation). https://www.nvidia.com/en-us/data-center/solutions/confidential-computing/
- [R19] Microsoft confidential-ai-workshop tutorial — confidential GPU VMs, MAA attestation, key release policy. https://github.com/microsoft/confidential-ai-workshop/tree/main/tutorials/confidential-llm-inferencing
- [R20] *Confidential Computing on NVIDIA Hopper GPUs* (as R8).
- [R21] ETH SPCL: *Confidential LLM Inference: Performance and Cost* — CPU TEE <10% throughput / <20% latency; H100 CC 4–8%. https://spcl.inf.ethz.ch/Publications/.pdf/chrapek-confidential.pdf
- [R22] *PipeLLM* (arXiv:2411.03357): up to 52.8–88.2% throughput drop in specific TEE configurations (honest caveat). https://arxiv.org/abs/2411.03357
- [R23] *Performance Analysis of Nvidia H100 Confidential Computing for AI* (2024): worst-case throughput drops up to 93%, mitigated runtime <28%. https://www.researchgate.net/publication/389188106_Performance_Analysis_and_Optimization_of_Nvidia_H100_Confidential_Computing_for_AI_Workloads
- [R24] Apple Private Cloud Compute — confidential computing in Apple-operated data centers (AMD + NVIDIA CC GPUs); attestation root is Apple. https://blog.freedomtechhq.com/inside-apples-private-cloud-compute-can-confidential-ai-be-trusted-4279d494d9b3
- [R25] Tinfoil: *How Does Tinfoil Compare to Apple Private Cloud Compute?* (2025) — trust-boundary and verification analysis. https://tinfoil.sh/blog/2025-01-30-how-do-we-compare
- [R26] *Encrypted Llama-3-8B decode of one token* (arXiv:2611.04488, Nov 2026): state-of-the-art FHE decode benchmark; perplexity 7.77 vs 7.76 plaintext; note timings composed per block, not end-to-end. https://arxiv.org/abs/2611.04488
- [R27] **EncryptedLLM** — de Castro et al. (PMLR v267, 2025): GPU-accelerated FHE GPT-2 forward, ~200× over CPU baseline. https://proceedings.mlr.press/v267/de-castro25a.html
