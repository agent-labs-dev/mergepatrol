-- A fork PR's head when a label went on it, from the `labeled` webhook: the code a maintainer
-- queued, which the queue may not see until its next poll.
CREATE TABLE label_heads (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    pr BIGINT NOT NULL,
    label TEXT NOT NULL,
    head_sha TEXT NOT NULL,
    PRIMARY KEY (installation_id, repo, pr, label)
);
