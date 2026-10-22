---
name: add-api-endpoint
description: Use when adding or changing an Arilbox REST endpoint in arilbox-api. Covers spec-first design, request/response types, validation, auth and CSRF, error mapping, audit logging and the required tests.
---

# Adding an API endpoint

The REST API is the only way the UI (and any future CLI) talks to the daemon. Its shape is
part of the product, documented in `SPEC.md` §7. The spec and the code change together.

## Steps

### 1. Spec first

- Find the endpoint in `SPEC.md` §7. If it isn't there, **add it to the spec in the same change**
  (method, path, purpose, request, response, status codes).
- Base path `/api/v1`. Resources are plural nouns (`/containers/{name}/snapshots`); actions that
  aren't CRUD are `POST` sub-paths (`/containers/{name}/start`).

### 2. Types

- Request and response structs in `arilbox-api`, `serde` with `snake_case` fields.
- Requests use `#[serde(deny_unknown_fields)]`, so typos fail loudly with 400.
- Convert request strings into validated newtypes (`ContainerName`, …) from `arilbox-core`
  immediately; handlers never pass raw strings further.
- Timestamps are RFC 3339 UTC. Sizes use explicit units in the field name
  (`memory_mb`, `disk_gb`).
- Never include secrets in responses (password hashes, tokens, `secret` app settings).

### 3. Auth and CSRF

- Every endpoint requires a valid session **except** `POST /auth/login` and the first-run setup.
- Every mutating method (`POST`, `PATCH`, `PUT`, `DELETE`) requires the header
  `X-Arilbox-Request: 1`; missing → `403 csrf_failed`.
- Use the shared extractors/middleware; don't hand-check cookies in a handler.

### 4. Behaviour

- Fast reads (list, get, stats) respond directly with 200.
- Anything slow or host-changing becomes a **job** — follow the `add-job` skill — and returns
  `202 { "job_id": "…" }` after validation.
- Validation and conflict checks happen **before** creating a job.

### 5. Errors

Map crate errors to the standard body `{ "error": { "code", "message" } }`:

| Status | When | Example `code` |
|---|---|---|
| 400 | Invalid input | `invalid_name`, `invalid_request` |
| 401 | Not logged in | `unauthenticated` |
| 403 | CSRF header missing | `csrf_failed` |
| 404 | Unknown resource | `container_not_found` |
| 409 | Exists, wrong state, busy | `container_exists`, `container_running`, `target_busy` |
| 500 | Unexpected | `internal_error` (details only in logs, with `request_id`) |

Error `code`s are stable `snake_case` strings: the UI translates by code, so never rename one.
`message` is English, human-readable, and contains no stderr, paths or stack traces.

### 6. Audit log

Every mutating request writes an audit entry: user, action (`container.create`), target, time,
result. Failed logins are audited too.

### 7. Register the route

Add it to the router in `arilbox-api`, grouped with its resource, behind the auth layer.

### 8. Frontend client

If `web/` exists, add or update the typed call in `web/src/lib/api.ts` so the UI uses the same
request/response shapes.

## Tests (required)

Handler tests against `MockBackend` + in-memory SQLite, covering:

- [ ] Success response and body shape.
- [ ] 400 for an invalid name and for an unknown field.
- [ ] 401 without a session.
- [ ] 403 without `X-Arilbox-Request` (mutating endpoints only).
- [ ] 404 for an unknown resource.
- [ ] 409 for each conflict the endpoint can hit.
- [ ] Audit entry written (mutating endpoints only).
- [ ] For job endpoints: 202 with a `job_id`, and the job is visible in `GET /jobs/{id}`.

## Checklist before finishing

- [ ] `SPEC.md` §7 matches the implementation.
- [ ] No secret fields in any response.
- [ ] `cargo clippy -D warnings` and `cargo test --workspace` pass.
