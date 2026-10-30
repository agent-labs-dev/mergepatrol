-- The You tab's PRs: every open PR of every repo the App is installed on, and those merged or
-- closed lately, as GitHub last said. A webhook, or the dashboard's own write, bumps `changes`;
-- a read keeps the `changes` it began at, so a change during the read leaves it stale. Logins
-- are lowercase.
CREATE TABLE inbox_repos (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    -- When every open PR was last read, to catch what webhooks missed. NULL: never.
    indexed_at TIMESTAMPTZ,
    PRIMARY KEY (installation_id, repo)
);

CREATE TABLE inbox_pulls (
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    number BIGINT NOT NULL,
    changes BIGINT NOT NULL DEFAULT 1,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read BIGINT NOT NULL DEFAULT 0,
    read_at TIMESTAMPTZ,
    -- `open`, `merged` or `closed`. NULL: never read.
    state TEXT,
    author TEXT,
    -- Asked to review and have not yet: users, and teams by slug.
    requested_users TEXT[] NOT NULL DEFAULT '{}',
    requested_teams TEXT[] NOT NULL DEFAULT '{}',
    -- Who reviewed it, by their latest reviews.
    reviewers TEXT[] NOT NULL DEFAULT '{}',
    closed_at TIMESTAMPTZ,
    node JSONB,
    PRIMARY KEY (installation_id, repo, number),
    FOREIGN KEY (installation_id, repo) REFERENCES inbox_repos ON DELETE CASCADE
);
CREATE INDEX inbox_pulls_stale ON inbox_pulls (changed_at) WHERE changes > read;
CREATE INDEX inbox_pulls_author ON inbox_pulls (author);
CREATE INDEX inbox_pulls_requested ON inbox_pulls USING GIN (requested_users);

-- Each team's members, child teams' members too, by the team's slug.
CREATE TABLE inbox_teams (
    installation_id BIGINT NOT NULL,
    team TEXT NOT NULL,
    login TEXT NOT NULL,
    PRIMARY KEY (installation_id, team, login)
);
CREATE INDEX inbox_teams_login ON inbox_teams (login);

-- When a user last opened a PR in the dashboard: what came after is unread.
CREATE TABLE inbox_seen (
    login TEXT NOT NULL,
    installation_id BIGINT NOT NULL,
    repo TEXT NOT NULL,
    number BIGINT NOT NULL,
    at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (login, installation_id, repo, number)
);
