-- When the review asked a human, and when one answered: how long the PR waited on a person.
-- What became of each finding is in `findings`, as its `outcome`.
ALTER TABLE agent_reviews ADD COLUMN asked_at TIMESTAMPTZ, ADD COLUMN answered_at TIMESTAMPTZ;
