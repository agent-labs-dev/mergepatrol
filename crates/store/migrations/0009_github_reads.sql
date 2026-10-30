-- What the queue last read of each repo it runs from GitHub, so it reads only what changed
-- since. A webhook, or the queue's own write, bumps `changes`. A read keeps the `changes` it
-- began at, so a change during the read leaves it stale.
CREATE TABLE github_repos (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    -- Changes to the repo itself: its default branch, its labels.
    changes BIGINT NOT NULL DEFAULT 0,
    -- The config file on the default branch: NULL, none. `config_read` NULL: never read.
    config_read BIGINT,
    config_read_at TIMESTAMPTZ,
    config TEXT,
    -- The repository as the snapshot reads it, without its PRs, for the queue labels `labels`.
    repository_read BIGINT,
    repository_read_at TIMESTAMPTZ,
    labels JSONB,
    repository JSONB,
    PRIMARY KEY (installation_id, repo)
);

-- The repo's PRs that are open with the trigger label, as the snapshot reads them, and the PRs
-- that a webhook said changed since. A PR read that is not queued is deleted.
CREATE TABLE github_prs (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    number BIGINT NOT NULL,
    changes BIGINT NOT NULL DEFAULT 1,
    read BIGINT NOT NULL DEFAULT 0,
    -- NULL: never read.
    node JSONB,
    PRIMARY KEY (installation_id, repo, number),
    FOREIGN KEY (installation_id, repo) REFERENCES github_repos ON DELETE CASCADE
);
