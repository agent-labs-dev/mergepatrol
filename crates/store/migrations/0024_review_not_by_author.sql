-- The author's agent no longer reviews: MergeLoop's own agent takes each review that waited for it.
-- `brief` and `submitted` stay while the pod this replaces still selects them in a rolling update;
-- a later release, once no deployed reader names them, drops them.
UPDATE agent_reviews SET status = 'pending', claimed_at = NULL, updated_at = now() WHERE status = 'author';
