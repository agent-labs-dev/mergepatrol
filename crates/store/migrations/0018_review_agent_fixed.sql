-- The queue's agent fixed what this review sent back: it spent none of the author's rounds.
ALTER TABLE agent_reviews ADD COLUMN agent_fixed BOOLEAN NOT NULL DEFAULT false;
