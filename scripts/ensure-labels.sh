#!/usr/bin/env bash
# Create (or update) the OpenPal ticket label taxonomy. Idempotent: safe to
# re-run any time, and the only way to add a label to the taxonomy is to add a
# line here and re-run it.
#
#   ./scripts/ensure-labels.sh
#
# Why this has to run BEFORE any ticket is filed: `gh issue create --label X`
# resolves every label to a node id up front, so one unknown label aborts the
# create and NO issue is produced. (The web issue forms fail the other way —
# an unknown label in a form's `labels:` key is a silent no-op — so the two
# paths disagree unless the whole taxonomy exists first.)
#
# `--force` is what makes this idempotent; without it `gh label create` errors
# out on a label that already exists.
set -euo pipefail

if ! gh auth status >/dev/null 2>&1; then
  echo "error: gh is not authenticated. Run: gh auth login" >&2
  exit 1
fi

# label  colour  description
labels=(
  # -- type: what kind of work this is. Exactly one per ticket, required. ----
  "type/feature|1d76db|New user-visible capability. Filed as a user story."
  "type/bug|d73a4a|Something behaves differently than it should."
  "type/chore|5319e7|Refactor, dependency, CI, build or cleanup work."
  "type/spike|0e8a16|Timeboxed investigation that produces a written finding, not code."

  # -- area: which part of OpenPal. Exactly one per ticket, required. --------
  "area/ui|006b75|Frontend shell, navigation, shared components, CSS design system, store plumbing."
  "area/chat|006b75|Conversations: composer, streaming render, sidebar, projects, attachments, export, skills, writing tools."
  "area/engine|006b75|llama.cpp subprocess: spawn/health/stop, SSE streaming, sampling, engine updates, hardware probing."
  "area/models|006b75|Catalog, fit and recommendation, HF search, custom models, downloads, Models and Discover screens."
  "area/agent|006b75|Agent mode: tool-calling loop, built-in tools, sandbox, consent, workspace jailing, subagents, audit."
  "area/web-search|006b75|Web search turn: SearXNG sidecar, web_search and fetch_url tools, extraction, reranking, citations."
  "area/sync|006b75|Multi-device conversation sync: HLC merge, outbox, mailbox, pairing, roster, relay server."
  "area/remote-access|006b75|Serving this desktop's engine to another device: gateway, pairing, remote client, discovery, WOL."
  "area/security|006b75|Keys and locks: SQLCipher, secret store, recovery codes, device identity, app lock, parental controls."
  "area/plugins|006b75|Plugin system: manifest contract, discovery, Python venv, sidecar lifecycle, Add-ons UI, Voice plugin."
  "area/app|006b75|Tauri app layer: IPC command surface, AppState and lifecycle, logging, settings, sign-in, in-app updates."
  "area/build|006b75|Bundler, Tauri packaging and signing, the portable SKU, Android bring-up, CI workflows."
  "area/docs|006b75|README, ARCHITECTURE, CONTRIBUTING, docs/*, plugin contract prose, AGENTS.md."
  "area/i18n-a11y|006b75|Translations (en/fr and the split bundles) and accessibility: focus, contrast, keyboard nav, screen readers."

  # -- priority: decided at filing, changed freely afterwards. ---------------
  "priority/p0|b60205|Drop everything. Data loss, crash on launch, or the app is unusable."
  "priority/p1|d93f0b|Next up. Blocks a release or a user's main path."
  "priority/p2|fbca04|Normal. The default for planned work."
  "priority/p3|fef2c0|Someday. Nice to have, no deadline."

  # -- status: the ticket's live state. Lives ONLY in labels, never in the ---
  # -- body, so a state change is never a body rewrite. ----------------------
  "status/triage|ededed|Filed quickly and not yet enriched. Do not start work on it."
  "status/ready|0e8a16|Fully specified. Safe to say 'implement ticket #N'."
  "status/in-progress|1d76db|An agent or human has started. Do not pick it up."
  "status/blocked|d93f0b|Waiting on a decision, a dependency or another ticket."
  "status/needs-repro|fbca04|Cannot be reproduced yet. Needs steps or evidence."
  "status/needs-rework|d93f0b|Implementation found the ticket wrong. See the latest Correction comment."

  # -- markers: applied only when true. --------------------------------------
  "epic|5319e7|Too large for one PR. Split into sub-issues; this is the parent."
  "severity/data-loss|b60205|This can destroy or corrupt user data, whatever its priority."
  "philosophy-override|d876e3|Deliberately departs from beginner-first or mono-model. The ticket records why."
  "no-ticket|ededed|Pull request that deliberately has no issue behind it (a revert, or repo bootstrap)."
)

for entry in "${labels[@]}"; do
  IFS='|' read -r name colour desc <<<"$entry"
  gh label create "$name" --color "$colour" --description "$desc" --force >/dev/null
  echo "  ok  $name"
done

echo
echo "Done — ${#labels[@]} labels ensured on $(gh repo view --json nameWithOwner --jq .nameWithOwner)."
