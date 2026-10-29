-- Hosted billing. Public repos are free, within a fair-use limit. Private repos need a Stripe
-- subscription, after its trial, priced per seat.

-- Each workspace's Stripe subscription, as Stripe last told. Times are Unix seconds.
CREATE TABLE billing (
    installation_id BIGINT PRIMARY KEY,
    stripe_customer TEXT NOT NULL,
    stripe_subscription TEXT NOT NULL,
    -- The subscription's one item: the seats.
    stripe_item TEXT NOT NULL,
    -- The subscription's Stripe status: `trialing`, `active`, `past_due`, `canceled`, and so on.
    status TEXT NOT NULL,
    -- The seats it bills for. The seats below change it.
    quantity INT NOT NULL,
    trial_end BIGINT
);

-- A seat: a developer whose PR joined a private repo's queue. It stays until freed in the
-- dashboard.
CREATE TABLE seats (
    installation_id BIGINT NOT NULL,
    login TEXT NOT NULL,
    added_at BIGINT NOT NULL,
    PRIMARY KEY (installation_id, login)
);

-- What the fair-use limit counts on public repos: each PR that joined a queue (`pr`), and each
-- job (`job`). Only the last hour matters: older rows are pruned.
CREATE TABLE fair_use (
    installation_id BIGINT NOT NULL,
    kind TEXT NOT NULL,
    at BIGINT NOT NULL
);
CREATE INDEX fair_use_by_installation ON fair_use (installation_id, kind, at);
