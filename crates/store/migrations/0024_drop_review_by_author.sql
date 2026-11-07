-- The author's agent no longer reviews: MergeLoop's own agent takes each review that waited for it.
UPDATE agent_reviews SET status = 'pending', claimed_at = NULL, updated_at = now() WHERE status = 'author';
ALTER TABLE agent_reviews DROP COLUMN brief, DROP COLUMN submitted;
