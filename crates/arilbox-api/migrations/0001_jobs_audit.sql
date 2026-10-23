-- Jobs (SPEC.md §8). Rows are written by the job runner only.
CREATE TABLE jobs (
    id          TEXT PRIMARY KEY,      -- UUIDv7, so ORDER BY id is creation order
    kind        TEXT NOT NULL,
    target      TEXT NOT NULL,
    status      TEXT NOT NULL,         -- queued | running | succeeded | failed | cancelled
    steps       TEXT NOT NULL,         -- JSON array of step records
    error       TEXT,                  -- JSON { code, message, step, rollback_incomplete }
    created_at  TEXT NOT NULL,         -- RFC 3339 UTC
    started_at  TEXT,
    finished_at TEXT
);
CREATE INDEX jobs_status ON jobs (status);

-- Audit log (SPEC.md §12): every mutating API request.
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          TEXT NOT NULL,         -- RFC 3339 UTC
    username    TEXT,                  -- NULL until sessions exist (M3)
    action      TEXT NOT NULL,         -- e.g. container.create
    target      TEXT,
    status      INTEGER NOT NULL,      -- HTTP status of the response
    request_id  TEXT
);
