CREATE TABLE lanes (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    trunk TEXT NOT NULL,
    lane JSONB NOT NULL,
    PRIMARY KEY (installation_id, repo, trunk)
);

CREATE TABLE events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    pr BIGINT NOT NULL,
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind TEXT NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX events_by_pr ON events (repo, pr, id);

CREATE TABLE repos (
    repo TEXT PRIMARY KEY,
    installation_id BIGINT NOT NULL,
    enabled BOOLEAN NOT NULL
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    login TEXT,
    avatar_url TEXT,
    expires_at BIGINT NOT NULL
);

CREATE TABLE secrets (
    name TEXT PRIMARY KEY,
    nonce BYTEA NOT NULL,
    ciphertext BYTEA NOT NULL,
    hint TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
