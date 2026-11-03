-- Each PR's node now has its repo's default branch, for the queue page's open PRs. When a full
-- read last gave every open node one: only this binary's index sets it, so a binary from before
-- it, still indexing during a rollout, does not make the repo look read. NULL: not yet, and the
-- page reads GitHub.
ALTER TABLE inbox_repos ADD COLUMN open_indexed_at TIMESTAMPTZ;
