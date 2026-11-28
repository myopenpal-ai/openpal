<!--
Every change to OpenPal is linked to an issue. Replace the number below; the
`Closes #N` line is what makes GitHub close the ticket on merge and link the PR
to it in the issue's timeline. Nothing in CI checks for it — a reviewer does.

No ticket yet? File one first from the issue forms (Issues → New issue). If
this PR genuinely should not have one (a revert, a typo fix), say why here and
a maintainer will add the `no-ticket` label.
-->

Closes #

## What changed

<!-- One paragraph. What a reviewer needs to know before reading the diff. -->

## How it was verified

<!--
Which of these you actually ran, and what happened. Say so plainly if a check
was skipped or could not run — do not imply a check you did not run.

  cargo fmt --check
  cargo clippy --workspace -- -D warnings
  cargo test -p openpal-core   (and -p openpal-app, -p openpal-bundler, -p openpal-sync-protocol)
  cd ui && npx tsc --noEmit && npx vite build && npm test && npm run test:e2e

Pull-request CI compiles Linux only. If this touches platform-gated code, say
which platform you built it on.
-->

## Ticket checklist

- [ ] A **self-review** comment is posted on the PR (what changed + how it was
  verified), per docs/SECURITY_POSTURE.md — the project's documented review
  ritual while it is a solo-maintainer repo.
- [ ] The `Closes #N` line above names the issue this PR implements — or the `no-ticket` label is set and the reason is above.
- [ ] If user-facing code changed (outside `docs/`, `*.md`, `.github/`, `scripts/`), this PR bumps the version and adds its changelog line (`node scripts/bump-version.mjs --bump … --notes "…"`) — or carries `[no release]` in its title. The `version-bump` check enforces this (and applies the bump itself on in-repo branches if it is forgotten).
- [ ] Every `Done when` line on the linked issue is satisfied, or the ones that are not are called out above.
- [ ] Nothing in `Not in scope` was touched.
- [ ] New or changed user-facing strings have both `en` and `fr` keys (`ui/src/i18n/keys.test.ts` passes).
