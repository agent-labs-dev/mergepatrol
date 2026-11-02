-- What a workspace sets for every repo that leaves it out of its config file. No row follows the
-- instance's defaults.
CREATE TABLE repo_defaults (
    installation_id BIGINT PRIMARY KEY,
    -- `WorkspaceDefaults`.
    settings JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
