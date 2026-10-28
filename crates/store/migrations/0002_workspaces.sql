-- A workspace is one installation of the GitHub App. Repo settings and secrets belong to one.

CREATE TABLE workspaces (
    installation_id BIGINT PRIMARY KEY,
    account TEXT NOT NULL,
    account_type TEXT NOT NULL,
    avatar_url TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE repos DROP CONSTRAINT repos_pkey;
ALTER TABLE repos ADD PRIMARY KEY (installation_id, repo);

-- `instance` for the self-hosted GitHub App, `installation:<id>` for a workspace's secrets.
ALTER TABLE secrets ADD COLUMN scope TEXT NOT NULL DEFAULT 'instance';
ALTER TABLE secrets ALTER COLUMN scope DROP DEFAULT;
ALTER TABLE secrets DROP CONSTRAINT secrets_pkey;
ALTER TABLE secrets ADD PRIMARY KEY (scope, name);

-- The installations the user could access when they signed in.
ALTER TABLE sessions ADD COLUMN installations BIGINT[] NOT NULL DEFAULT '{}';

CREATE INDEX events_by_installation ON events (installation_id, repo, pr, id);
