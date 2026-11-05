-- Where the author's agent reviews (`agent.runs: local`): what MergeLoop asks of it, and the review
-- it posts, which MergeLoop then judges as it does its own.
ALTER TABLE agent_reviews ADD COLUMN brief TEXT, ADD COLUMN submitted JSONB;
