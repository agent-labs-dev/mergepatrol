-- When Mergepatrol told the PR, on GitHub, that its review's asks are answered on the
-- dashboard, not in a comment: once per review.
ALTER TABLE agent_reviews ADD COLUMN nudged_at TIMESTAMPTZ;
