# Contract changelog

One line per change to the frozen `/v1` surface. A change here is a cross-repo
event: the app repo's parser tests must pass against the new fixture before this
entry is added.

Format: `date · route · change · compatible with shipped clients?`

## 2026-11-25 — Phase 0 freeze

- `POST /v1/desktop/session` · initial fixture · yes
- `GET /v1/desktop/session/{id}` · initial fixtures (pending, complete, expired,
  denied); tokens are snake_case in the poll, camelCase everywhere else · yes
- `POST /v1/auth/refresh` · initial fixtures; response carries `accessToken`
  only (rotation off) · yes
- `POST /v1/auth/signout` · no fixture (any 2xx is accepted) · yes
- `GET /v1/account/me` · two fixtures: Plus with quota, Free without a `quota`
  key · yes
- `GET /v1/cloud-models` · bare array with the two launch models · yes
- `POST /v1/chat/completions` · three SSE fixtures: plain, reasoning, error ·
  yes
- Error envelope · three fixtures (401, 403, 429) · yes

Not yet frozen here, because the surface does not exist yet:

- `/v1/sync/*` — lands with Phase 4. The vault wire format lives in the public
  protocol crate (`crates/sync-protocol` in `myopenpal-ai/openpal`), with its
  own fixtures and changelog, not in `contract/v1/`. The private backend
  implements that crate; it does not define it (`docs/SYNC_MODEL.md` §6.4,
  ADR-015). The vault protocol is still deliberately NOT pinned before the
  crypto review (see `docs/DECISIONS.md` O5); adding a fixture now would freeze
  a shape we expect to change.
- `/v1/managed-search/search` — request/response shape is pinned in
  `v1/search_results.json`; the fake serves it from Phase 3.
- `/v1/billing/*`, `/v1/account/devices`, `/v1/account/usage` — additive, no
  shipped client reads them yet.
