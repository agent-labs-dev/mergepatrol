-- A user's session keeps their GitHub token, encrypted with the secret key, to read their
-- installations again while the session lasts.
ALTER TABLE sessions ADD COLUMN github_nonce BYTEA;
ALTER TABLE sessions ADD COLUMN github_ciphertext BYTEA;
-- When the installations were last read from GitHub, in Unix seconds.
ALTER TABLE sessions ADD COLUMN checked_at BIGINT NOT NULL DEFAULT 0;
-- A request reading them again holds the session until then, so only one request at a time
-- spends the refresh token, which GitHub replaces on each use.
ALTER TABLE sessions ADD COLUMN checking_until BIGINT NOT NULL DEFAULT 0;
