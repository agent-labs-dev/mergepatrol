-- The approved head a review goes on from: the queue's agent changed the PR since, and only its
-- changes need reading. NULL reads the whole PR.
ALTER TABLE agent_reviews ADD COLUMN since_head TEXT;
