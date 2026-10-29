-- A one-time code for the menu bar app's sign-in. The browser carries only the code to the
-- app; the app trades it, with its PKCE verifier, for the session token sealed here.
CREATE TABLE login_codes (
    code_hash TEXT PRIMARY KEY,
    -- The session the code hands over. Deleted with the code if nobody takes it.
    token_hash TEXT NOT NULL,
    -- The app's S256 challenge, base64url.
    challenge TEXT NOT NULL,
    token_nonce BYTEA NOT NULL,
    token_ciphertext BYTEA NOT NULL,
    expires_at BIGINT NOT NULL
);
