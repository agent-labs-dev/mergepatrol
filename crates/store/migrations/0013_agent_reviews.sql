-- The agent's reviews of queued PRs, one per PR head: what it found, which findings it asks a
-- human about, and what the human answered. A push gives the PR a new head, and a new review.
CREATE TABLE agent_reviews (
    id BIGSERIAL PRIMARY KEY,
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    number BIGINT NOT NULL,
    head_sha TEXT NOT NULL,
    -- `pending`, `running`, `asks`, `approved`, `changes` or `stale`.
    status TEXT NOT NULL DEFAULT 'pending',
    -- Why the agent could not review: a human reviews the whole PR instead.
    error TEXT,
    -- Failed runs so far, of errors outside the PR.
    attempts INT NOT NULL DEFAULT 0,
    summary TEXT NOT NULL DEFAULT '',
    -- `[Finding]`, each with `asked`, and the bar that applied.
    findings JSONB NOT NULL DEFAULT '[]',
    -- Who the agent asked, by login.
    asked TEXT[] NOT NULL DEFAULT '{}',
    -- `{finding id: Answer}`.
    answers JSONB NOT NULL DEFAULT '{}',
    answered_by TEXT,
    note TEXT NOT NULL DEFAULT '',
    -- `[Question]`: a human's questions about findings, and the agent's answers.
    questions JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the agent's worker last took it, to retry a run a restart cut short.
    claimed_at TIMESTAMPTZ,
    -- When a run may be tried again, after an error.
    retry_at TIMESTAMPTZ,
    UNIQUE (installation_id, repo, number, head_sha)
);
CREATE INDEX agent_reviews_pr ON agent_reviews (installation_id, repo, number, id DESC);
CREATE INDEX agent_reviews_todo ON agent_reviews (id) WHERE status IN ('pending', 'running');
CREATE INDEX agent_reviews_recent ON agent_reviews (installation_id, created_at);

-- A workspace's review settings. A workspace without a row has the defaults.
CREATE TABLE review_settings (
    installation_id BIGINT PRIMARY KEY,
    settings JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
