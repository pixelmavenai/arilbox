-- Users and sessions (SPEC.md §12). v1 has a single admin user, created by first-run setup.
CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,         -- argon2id PHC string
    created_at    TEXT NOT NULL,         -- RFC 3339 UTC
    updated_at    TEXT NOT NULL
);

-- Server-side sessions. The cookie holds a random 256-bit ID; only its SHA-256 is stored, so
-- a leaked database does not leak usable sessions.
CREATE TABLE sessions (
    id_hash      TEXT PRIMARY KEY,       -- hex SHA-256 of the session ID
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,       -- Unix seconds (absolute timeout)
    last_seen_at INTEGER NOT NULL        -- Unix seconds (idle timeout)
);
CREATE INDEX sessions_user ON sessions (user_id);
