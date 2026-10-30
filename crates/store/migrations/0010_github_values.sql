-- When each repo and PR last changed: GitHub's reads lag its webhooks by seconds, so a read soon
-- after a change is not kept.
ALTER TABLE github_repos ADD COLUMN changed_at TIMESTAMPTZ;
ALTER TABLE github_prs ADD COLUMN changed_at TIMESTAMPTZ;

-- The queue's other reads of a repo, by key: `checks:<sha>`, `ref:<branch>`, `pull:<pr>`, and
-- so on. As in `github_repos`, a change bumps `changes`, and a read keeps the `changes` it began
-- at. A key gets its row when the queue first reads it, or a webhook says it changed.
CREATE TABLE github_values (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    key TEXT NOT NULL,
    changes BIGINT NOT NULL DEFAULT 0,
    changed_at TIMESTAMPTZ,
    -- `changes` when `value` was read. NULL: never read.
    read BIGINT,
    read_at TIMESTAMPTZ,
    value JSONB,
    PRIMARY KEY (installation_id, repo, key),
    FOREIGN KEY (installation_id, repo) REFERENCES github_repos ON DELETE CASCADE
);

-- When the reads under each key prefix last changed together, as after a publish moved the
-- branches: a key read for the first time since gets its `changed_at`.
CREATE TABLE github_value_prefixes (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    prefix TEXT NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (installation_id, repo, prefix),
    FOREIGN KEY (installation_id, repo) REFERENCES github_repos ON DELETE CASCADE
);
