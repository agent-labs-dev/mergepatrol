-- A PR a user put out of their inbox until `until`, or until someone else says something on it
-- after `at`.
CREATE TABLE inbox_snoozed (
    login TEXT NOT NULL,
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    number BIGINT NOT NULL,
    at TIMESTAMPTZ NOT NULL,
    until TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (login, installation_id, repo, number)
);
