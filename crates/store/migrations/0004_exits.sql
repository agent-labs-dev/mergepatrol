-- Each time a PR left the queue, how it left, and the PR as it was then: the queue's history,
-- and what its metrics count. `outcome` is the event kind: `merged`, `ejected`, `dequeued` or
-- `cancelled`. `detail` is the merge commit, or why.
CREATE TABLE exits (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    trunk TEXT NOT NULL,
    pr BIGINT NOT NULL,
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL,
    summary JSONB NOT NULL,
    -- NULL for a PR ejected before it joined, such as one from a fork.
    enqueued_at TIMESTAMPTZ,
    -- Seconds in each step of the queue, from the PR's events: `waiting`, `held`, `rebasing`,
    -- `checks` and `fixing`. NULL with `enqueued_at`.
    steps JSONB,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX exits_by_installation ON exits (installation_id, repo, id);
