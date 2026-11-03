-- Which taking of the review a worker holds: it writes its result only while that is the latest.
ALTER TABLE agent_reviews ADD COLUMN claim BIGINT NOT NULL DEFAULT 0;
