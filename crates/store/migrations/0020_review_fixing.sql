-- The review sent findings back that the queue's agent fixes first: nothing is posted yet. A
-- restart that cuts the fix short runs the review again.
ALTER TABLE agent_reviews ADD COLUMN fixing BOOLEAN NOT NULL DEFAULT false;
