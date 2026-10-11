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
  cargo test -p openpal-core   (and -p openpal-app, -p openpal-bundler)
  cd ui && npx tsc --noEmit && npx vite build && npm test && npm run test:e2e

Pull-request CI compiles Linux only. If this touches platform-gated code, say
which platform you built it on.
-->

## Ticket checklist

- [ ] The `Closes #N` line above names the issue this PR implements — or the `no-ticket` label is set and the reason is above.
- [ ] Every `Done when` line on the linked issue is satisfied, or the ones that are not are called out above.
- [ ] Nothing in `Not in scope` was touched.
- [ ] New or changed user-facing strings have both `en` and `fr` keys (`ui/src/i18n/keys.test.ts` passes).
