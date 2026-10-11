# Design records

These documents are the plans, research and audits behind OpenPal features.
They're kept because the code cites them for the reasoning behind a decision,
and because the roadmap ones show where the project is heading.

Read them as **point-in-time records, not specs**:

- Some are fully implemented, some partly, some not at all. The code is the
  source of truth for what exists; the reference docs one level up
  ([docs/](../)) describe current behavior.
- File paths and line numbers inside them were correct when written and drift
  as the code moves.

| Document | What it is |
| --- | --- |
| [ACCESSIBILITY_AUDIT.md](ACCESSIBILITY_AUDIT.md) | WCAG 2.2 AA / RGAA audit of the UI |
| [ACCESSIBILITY_PLAN.md](ACCESSIBILITY_PLAN.md) | Remediation plan for the audit's findings |
| [DISK_OFFLOAD.md](DISK_OFFLOAD.md) | Running larger-than-RAM MoE models by streaming weights from disk |
| [HF_BROWSER.md](HF_BROWSER.md) | The Hugging Face model browser plugin |
| [MOBILE.md](MOBILE.md) | Extending OpenPal to Android and iOS (roadmap) |
| [MULTI_DEVICE_SYNC.md](MULTI_DEVICE_SYNC.md) | Syncing conversations across devices |
| [PORTABLE.md](PORTABLE.md) | The no-install Portable build for Windows, Linux and macOS |
| [PROMPT_ENHANCER.md](PROMPT_ENHANCER.md) | "Improve my message", the one-shot draft rewrite |
| [SPEED_ESTIMATE_ACCURACY.md](SPEED_ESTIMATE_ACCURACY.md) | Making the tokens/second estimate match reality |
| [WRITING_TOOLS.md](WRITING_TOOLS.md) | The writing tools plugin |
