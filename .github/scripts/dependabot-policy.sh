#!/usr/bin/env bash
#
# Dependabot auto-merge policy engine (issue #84, docs/SECURITY_POSTURE.md).
#
# Decides whether an open Dependabot PR may auto-merge, against the tracked
# policy in .github/dependabot-policy.json, and applies the verdict:
#
#   PASS -> ensure label `automerge-ok`, request auto-merge (squash)
#   FAIL -> ensure label `needs-human-review`, cancel any pending auto-merge
#
# Gates applied to EVERY dependency the PR touches (one failure blocks the PR):
#   1. blocklist         - key `cargo/rustls`-style, absolute, never auto-merges
#   2. allowlist         - fail-closed: only owner-vetted entries may auto-merge
#   3. known publisher   - publisher/owner or a package maintainer is in
#                          `trusted_publishers` (pip has no owner API on the
#                          public index, so its publisher gate IS the allowlist)
#   4. major bump        - major segment changes, plus 0.x minor bumps (Rust
#                          convention: 0.17 -> 0.23 and 0.10 -> 0.11 are
#                          breaking, see issue #84 plan)
#   5. release age       - the new version must have been published at least
#                          its category's window ago (dev/build 3-7d, runtime
#                          7-14d, critical ~30d; see windows_days)
#
# The sweep path (schedule / workflow_dispatch in dependabot-automerge.yml)
# loops this over every open Dependabot PR, which is what flips an age-gated PR
# to auto-merge once its release window passes.
#
# Usage:
#   dependabot-policy.sh --pr <N> [--dry-run] [--repo owner/repo] [--ref main]
#
# Required env: GH_TOKEN. Optional: GH_REPO (or --repo). `--ref` selects which
# branch the policy file is read from (default main).
#
# Exit codes:
#   0  evaluated (verdict PASS or FAIL was applied - FAIL is a valid outcome,
#      communicated by the `needs-human-review` label, not an error)
#   1  hard error (unreachable policy, missing tools) - fail-closed, no action
#   2  not a Dependabot PR (skip)
set -euo pipefail

PR=""
DRY_RUN=0
REPO="${GH_REPO:-}"
REF="main"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pr) PR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --repo) REPO="$2"; shift 2 ;;
    --ref) REF="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

[[ -n "$PR" ]] || { echo "::error::--pr is required" >&2; exit 1; }
[[ -n "$REPO" ]] || { echo "::error::GH_REPO is required" >&2; exit 1; }
[[ -n "${GH_TOKEN:-}" ]] || { echo "::error::GH_TOKEN is required" >&2; exit 1; }
for tool in gh jq curl python3; do
  command -v "$tool" >/dev/null 2>&1 || { echo "::error::missing tool: $tool" >&2; exit 1; }
done

log() { printf '%s\n' "$*"; }
warn() { printf '::warning::%s\n' "$*"; }
step() { printf '::group::%s\n' "$*"; }
endstep() { printf '::endgroup::\n'; }

UA="openpal-dependabot-automerge (https://github.com/$REPO)"
NOW_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# --- resolve the PR -----------------------------------------------------------

PR_JSON="$(gh pr view "$PR" --json number,title,body,headRefName,author,labels \
    --jq '{n:.number,title,body,head:.headRefName,author:.author.login,labels:[.labels[].name]}' 2>/dev/null || true)"
if [[ -z "$PR_JSON" ]]; then
  echo "::error::could not read PR $PR" >&2
  exit 1
fi

AUTHOR="$(jq -r '.author' <<<"$PR_JSON")"
if [[ "$AUTHOR" != "app/dependabot" && "$AUTHOR" != "dependabot[bot]" ]]; then
  log "PR #$PR: author '$AUTHOR' is not Dependabot - skipping"
  exit 2
fi

TITLE="$(jq -r '.title' <<<"$PR_JSON")"
BODY="$(jq -r '.body' <<<"$PR_JSON")"
HEAD="$(jq -r '.head' <<<"$PR_JSON")"
LABELS="$(jq -r '.labels[]' <<<"$PR_JSON")"

step "PR #$PR: $TITLE"
log "head: $HEAD"

# --- policy (single source of truth) -----------------------------------------

# Prefer the checked-out policy file (the workflow checks out the base/main
# ref, so the working-tree copy IS the policy in effect); fall back to fetching
# it from the remote ref so the script also works without a checkout.
POLICY_JSON="$(cat .github/dependabot-policy.json 2>/dev/null || true)"
if [[ -z "$POLICY_JSON" ]]; then
  POLICY_JSON="$(gh api "repos/$REPO/contents/.github/dependabot-policy.json?ref=$REF" \
      --jq '.content' 2>/dev/null | base64 -d 2>/dev/null || true)"
fi
if [[ -z "$POLICY_JSON" ]] || ! jq -e '.trusted_publishers and .allowlist and .blocklist and .windows_days' >/dev/null 2>&1 <<<"$POLICY_JSON"; then
  echo "::error::could not read a valid .github/dependabot-policy.json (local or ref '$REF')" >&2
  exit 1
fi

TRUSTED="$(jq -r '.trusted_publishers[]' <<<"$POLICY_JSON")"

# --- ecosystem from the branch name ------------------------------------------

ECO=""
case "$HEAD" in
  dependabot/cargo/*) ECO="cargo" ;;
  dependabot/npm_and_yarn/*) ECO="npm" ;;
  dependabot/pip/*) ECO="pip" ;;
  dependabot/github_actions/*|dependabot/github-actions/*) ECO="github-actions" ;;
esac
if [[ -z "$ECO" ]]; then
  warn "PR #$PR: unrecognised Dependabot ecosystem in '$HEAD' - needs human review"
  ECO="unknown"
fi
log "ecosystem: $ECO"

# --- parse dependencies from the PR body -------------------------------------

# Dependabot bodies use one line per dependency, in either shape:
#   Bumps [rustls](https://github.com/rustls/rustls) from 0.23.40 to 0.23.46.
#   Updates `actions/upload-artifact` from 4.6.2 to 7.0.2
parse_deps() {
  local line name old new
  echo "$BODY" | while IFS= read -r line; do
    # shellcheck disable=SC2016 # the sed patterns are intentional, not expansions
    name="$(printf '%s\n' "$line" | sed -nE \
      -e 's#^([Bb]umps?|[Uu]pdates) `([^`]+)` from .*#\2#p' \
      -e 's#^([Bb]umps?|[Uu]pdates) \[([^]]+)\]\([^)]*\) from .*#\2#p')"
    [[ -n "$name" ]] || continue
    old="$(printf '%s\n' "$line" | sed -nE 's#^([Bb]umps?|[Uu]pdates) .+ from ([^ ]+) to .*#\2#p')"
    new="$(printf '%s\n' "$line" | sed -nE 's#^([Bb]umps?|[Uu]pdates) .+ from [^ ]+ to ([^ ]+).*#\2#p')"
    new="${new%[.,;]}"  # strip a trailing period/comma left by the punctuation
    printf '%s|%s|%s\n' "$name" "$old" "$new"
  done
}

DEPS="$(parse_deps || true)"
if [[ -z "$DEPS" ]]; then
  warn "PR #$PR: no 'Bumps/Updates ... from ... to ...' lines found in the body - needs human review"
fi

# --- helpers ------------------------------------------------------------------

# is_major OLD NEW - echoes 1 when the bump is a breaking major (incl. 0.x)
is_major() {
  python3 - "$1" "$2" <<'PY'
import re, sys
def num(v):
    v = re.sub(r'^[vV]', '', v)
    m = re.match(r'^(\d+)\.(\d+)(?:\.\d+)?', v)
    return [int(m.group(1)), int(m.group(2))] if m else None
old, new = num(sys.argv[1]), num(sys.argv[2])
if old is None or new is None:
    print(2)
    sys.exit(0)
if new[0] != old[0] or (old[0] == 0 and new[1] != old[1]):
    print(1)
else:
    print(0)
PY
}

# age_days PUBLISHED_ISO - echoes whole days since publication (UTC)
age_days() {
  python3 - "$1" "$2" <<'PY'
import sys
from datetime import datetime, timezone
pub = sys.argv[1].replace('Z', '+00:00')
dt = datetime.fromisoformat(pub)
if dt.tzinfo is None:
    dt = dt.replace(tzinfo=timezone.utc)
now = datetime.fromisoformat(sys.argv[2].replace('Z', '+00:00'))
if now.tzinfo is None:
    now = now.replace(tzinfo=timezone.utc)
print(int((now - dt).total_seconds() // 86400))
PY
}

# publisher_for ECO NAME - echoes the publisher identity, or empty when unknown
publisher_for() {
  case "$1" in
    cargo)
      local repo owner
      repo="$(curl -fsS -m 20 -A "$UA" "https://crates.io/api/v1/crates/$2" | jq -r '.crate.repository // empty' 2>/dev/null || true)"
      [[ -n "$repo" ]] || return 1
      owner="$(printf '%s\n' "$repo" | sed -nE 's#^https?://github.com/([^/]+)/.*#\1#p')"
      [[ -n "$owner" ]] || return 1
      printf '%s\n' "$owner"
      ;;
    npm)
      local url
      url="https://registry.npmjs.org/${2//\//%2F}"
      curl -fsS -m 20 "$url" | jq -r '.maintainers[].name' 2>/dev/null | while IFS= read -r m; do
        printf '%s\n' "$m"
      done
      ;;
    pip)
      # PyPI's public JSON exposes no stable owner identity; the fail-closed
      # allowlist entry IS this dependency's publisher gate (documented in
      # docs/SECURITY_POSTURE.md).
      ;;
    github-actions)
      printf '%s\n' "${2%%/*}"
      ;;
  esac
  return 0
}

# published_at ECO NAME NEW - echoes the publish timestamp, or empty
published_at() {
  case "$1" in
    cargo)
      curl -fsS -m 20 -A "$UA" "https://crates.io/api/v1/crates/$2/$3" \
        | jq -r '.version.created_at // empty' 2>/dev/null || true
      ;;
    npm)
      curl -fsS -m 20 "https://registry.npmjs.org/${2//\//%2F}" \
        | jq -r --arg v "$3" '.time[$v] // empty' 2>/dev/null || true
      ;;
    pip)
      curl -fsS -m 20 "https://pypi.org/pypi/$2/$3/json" \
        | jq -r '.urls[0].upload_time // empty' 2>/dev/null || true
      ;;
    github-actions)
      local owner repo tag out
      owner="${2%%/*}"
      repo="${2#*/}"
      for tag in "$3" "v$3"; do
        out="$(gh api "repos/$owner/$repo/releases/tags/$tag" --jq '.published_at // empty' 2>/dev/null || true)"
        [[ -n "$out" ]] && { printf '%s\n' "$out"; return 0; }
      done
      # No GitHub release for the tag - fall back to the tagged commit's date.
      local sha
      for tag in "$3" "v$3"; do
        sha="$(gh api "repos/$owner/$repo/git/matching-refs/tags/$tag" \
            --jq '.[0].object | (if .peeled then .peeled else .sha end) // empty' 2>/dev/null || true)"
        [[ -n "$sha" ]] || continue
        out="$(gh api "repos/$owner/$repo/commits/$sha" --jq '.commit.author.date // empty' 2>/dev/null || true)"
        [[ -n "$out" ]] && { printf '%s\n' "$out"; return 0; }
      done
      ;;
  esac
}

# --- evaluate every dependency -------------------------------------------------

VERDICT="PASS"
REASONS=""

note_fail() {  # $1 = reason
  VERDICT="FAIL"
  REASONS="${REASONS}    - $1"$'\n'
}

count=0
while IFS= read -r dep; do
  [[ -n "$dep" ]] || continue
  count=$((count + 1))
  name="${dep%%|*}"
  rest="${dep#*|}"
  old="${rest%%|*}"
  new="${rest#*|}"
  key="$ECO/$name"

  step "dependency #$count: $key  ($old -> $new)"
  log "policy key: $key"

  # 1. blocklist (absolute)
  if jq -e --arg k "$key" '.blocklist | index($k)' >/dev/null 2>&1 <<<"$POLICY_JSON"; then
    note_fail "$key is on the blocklist"
    endstep; continue
  fi

  # 2. allowlist (fail-closed)
  CATEGORY="$(jq -r --arg k "$key" '.allowlist[$k] // empty' <<<"$POLICY_JSON")"
  if [[ -z "$CATEGORY" ]]; then
    note_fail "$key is not on the allowlist"
    endstep; continue
  fi

  # 3. major bump (incl. 0.x minor bumps)
  MAJOR="$(is_major "$old" "$new")"
  if [[ "$MAJOR" == "1" ]]; then
    note_fail "$key is a major bump: $old -> $new"
    endstep; continue
  elif [[ "$MAJOR" == "2" ]]; then
    note_fail "$key has an unparseable version: $old -> $new"
    endstep; continue
  fi

  # 4. known publisher / maintainer. pip is the documented exception: PyPI's
  # public API exposes no owner identity, so the fail-closed allowlist entry
  # (gate 2) IS its publisher gate.
  PUBLISHER_TRUSTED=""
  if [[ "$ECO" == "pip" ]]; then
    PUBLISHER_TRUSTED="allowlist"
  else
    while IFS= read -r p; do
      [[ -n "$p" ]] || continue
      if printf '%s\n' "$TRUSTED" | grep -qxF "$p"; then
        PUBLISHER_TRUSTED="$p"
        break
      fi
    done < <(publisher_for "$ECO" "$name" || true)
  fi
  if [[ -z "$PUBLISHER_TRUSTED" ]]; then
    note_fail "$key publisher/maintainer is not in trusted_publishers"
    endstep; continue
  fi
  log "publisher: $PUBLISHER_TRUSTED (trusted)"

  # 5. release age vs its category window
  WINDOW="$(jq -r --arg c "$CATEGORY" '.windows_days[$c] // empty' <<<"$POLICY_JSON")"
  [[ -n "$WINDOW" ]] || WINDOW=10
  PUBLISHED="$(published_at "$ECO" "$name" "$new")"
  if [[ -z "$PUBLISHED" ]]; then
    note_fail "$key publish date for $new could not be determined"
    endstep; continue
  fi
  AGE="$(age_days "$PUBLISHED" "$NOW_ISO")"
  if (( AGE < WINDOW )); then
    note_fail "$key is only $AGE day(s) old (window $WINDOW days)"
    endstep; continue
  fi
  log "released $AGE days ago, window $WINDOW days (category $CATEGORY)"
  endstep
done <<<"$DEPS"

if (( count == 0 )); then
  note_fail "no dependencies were parseable from this PR"
fi

# --- apply the verdict ---------------------------------------------------------

log ""
if [[ "$VERDICT" == "PASS" ]]; then
  log "VERDICT: PASS - request auto-merge"
  if (( DRY_RUN )); then
    log "(dry-run) would label automerge-ok and request auto-merge"
  else
    gh label create automerge-ok --color 2da44e \
      --description "Dependabot PR passed the auto-merge policy" --force >/dev/null 2>&1 || true
    gh label create needs-human-review --color b60205 \
      --description "Dependabot PR blocked by the auto-merge policy - review required" --force >/dev/null 2>&1 || true
    if grep -qxF "needs-human-review" <<<"$LABELS"; then
      gh pr edit "$PR" --remove-label needs-human-review >/dev/null 2>&1 || true
    fi
    if ! grep -qxF "automerge-ok" <<<"$LABELS"; then
      gh pr edit "$PR" --add-label automerge-ok >/dev/null 2>&1 || true
    fi
    # Request auto-merge (squash). Failing is fine - a dirty tree, a draft, or
    # failing checks just mean the next evaluation (synchronize/sweep) retries.
    if gh pr view "$PR" --json autoMergeRequest --jq '.autoMergeRequest' 2>/dev/null | grep -q 'enableDate'; then
      log "auto-merge already requested"
    else
      if gh pr merge "$PR" --auto --squash >/dev/null 2>&1; then
        log "auto-merge requested"
      else
        warn "PR #$PR passed policy but auto-merge could not be requested yet (checks/conflicts) - sweep will retry"
      fi
    fi
  fi
else
  log "VERDICT: FAIL - needs human review:"
  printf '%s' "$REASONS"
  if (( DRY_RUN )); then
    log "(dry-run) would label needs-human-review and cancel auto-merge"
  else
    gh label create needs-human-review --color b60205 \
      --description "Dependabot PR blocked by the auto-merge policy - review required" --force >/dev/null 2>&1 || true
    gh label create automerge-ok --color 2da44e \
      --description "Dependabot PR passed the auto-merge policy" --force >/dev/null 2>&1 || true
    if grep -qxF "automerge-ok" <<<"$LABELS"; then
      gh pr edit "$PR" --remove-label automerge-ok >/dev/null 2>&1 || true
    fi
    if ! grep -qxF "needs-human-review" <<<"$LABELS"; then
      gh pr edit "$PR" --add-label needs-human-review >/dev/null 2>&1 || true
    fi
    # Cancel any pending auto-merge so a later policy change cannot slip it in.
    if gh pr view "$PR" --json autoMergeRequest --jq '.autoMergeRequest' 2>/dev/null | grep -q 'enableDate'; then
      node_id="$(gh api "repos/$REPO/pulls/$PR" --jq '.node_id' 2>/dev/null || true)"
      if [[ -n "$node_id" ]]; then
        # shellcheck disable=SC2016 # the GraphQL query is intentional, not an expansion
        gh api graphql -f query='mutation($id: ID!){ disablePullRequestAutoMerge(input:{pullRequestId:$id}){ clientMutationId } }' \
          -F "id=$node_id" >/dev/null 2>&1 || warn "could not cancel auto-merge on PR #$PR"
      fi
    fi
  fi
fi

endstep
log "exit: $VERDICT"
exit 0
