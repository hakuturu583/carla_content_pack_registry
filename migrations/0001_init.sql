-- CARLA Content Pack Registry — D1 (SQLite) schema.
-- GitHub numeric IDs are the identifiers; login/slug columns are display-only.

CREATE TABLE IF NOT EXISTS users (
  id          INTEGER PRIMARY KEY,           -- GitHub user id
  login       TEXT    NOT NULL,
  name        TEXT,
  avatar_url  TEXT,
  created_at  TEXT    NOT NULL,
  updated_at  TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS buckets (
  id                     TEXT    PRIMARY KEY,
  owner_id               INTEGER NOT NULL REFERENCES users(id),
  name                   TEXT    NOT NULL,
  description            TEXT    NOT NULL DEFAULT '',
  visibility             TEXT    NOT NULL CHECK (visibility IN ('public', 'private')),
  provider               TEXT    NOT NULL CHECK (provider IN ('r2', 's3', 'local')),
  storage_config         TEXT    NOT NULL,   -- JSON, no secrets
  credential_ciphertext  TEXT,               -- AES-GCM envelope (R2/local only)
  verify_status          TEXT    NOT NULL DEFAULT 'unverified',
  verify_detail          TEXT,
  verified_at            TEXT,
  created_at             TEXT    NOT NULL,
  updated_at             TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_buckets_owner ON buckets(owner_id);
CREATE INDEX IF NOT EXISTS idx_buckets_visibility ON buckets(visibility);

CREATE TABLE IF NOT EXISTS bucket_acl (
  bucket_id       TEXT    NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
  principal_type  TEXT    NOT NULL CHECK (principal_type IN ('user', 'org', 'team')),
  principal_id    INTEGER NOT NULL,
  principal_name  TEXT    NOT NULL,
  PRIMARY KEY (bucket_id, principal_type, principal_id)
);
CREATE INDEX IF NOT EXISTS idx_acl_principal ON bucket_acl(principal_type, principal_id);

CREATE TABLE IF NOT EXISTS sessions (
  id_hash           TEXT    PRIMARY KEY,     -- SHA-256 of the cookie value
  user_id           INTEGER NOT NULL,
  token_ciphertext  TEXT    NOT NULL,        -- encrypted GitHub access token
  csrf_token        TEXT    NOT NULL,
  expires_at        INTEGER NOT NULL,
  created_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

CREATE TABLE IF NOT EXISTS membership_cache (
  user_id     INTEGER PRIMARY KEY,
  payload     TEXT    NOT NULL,
  expires_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ts         TEXT    NOT NULL,
  actor_id   INTEGER,
  action     TEXT    NOT NULL,
  bucket_id  TEXT,
  outcome    TEXT    NOT NULL,
  detail     TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);
CREATE INDEX IF NOT EXISTS idx_audit_bucket ON audit_log(bucket_id, ts);
