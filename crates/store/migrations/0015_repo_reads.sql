-- What the queue last read of a repo that the dashboard and the API tell before a PR is queued:
-- whether it is private, which a subscription must cover, and the tasks its agent runs, each
-- with its provider, whose key the workspace must have. `private` NULL: never read.
ALTER TABLE repos ADD COLUMN private BOOLEAN;
ALTER TABLE repos ADD COLUMN agent JSONB NOT NULL DEFAULT '[]';
