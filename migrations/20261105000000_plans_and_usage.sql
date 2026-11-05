-- Each plan and its limits. The server reads the limits from this table, so a change here
-- needs no new release. NULL in a max_* column means no limit.
CREATE TABLE plans (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code                 TEXT NOT NULL UNIQUE CHECK (code ~ '^[a-z0-9_-]{1,32}$'),
    name                 TEXT NOT NULL,
    price_cents          INTEGER NOT NULL CHECK (price_cents >= 0),
    currency             TEXT NOT NULL DEFAULT 'usd',
    interval             TEXT CHECK (interval IN ('month', 'year')),
    -- The lookup key of the Stripe price. The webhook maps a subscription to a plan with it.
    stripe_lookup_key    TEXT UNIQUE,
    max_servers          INTEGER CHECK (max_servers >= 0),
    max_tools_per_server INTEGER CHECK (max_tools_per_server >= 0),
    max_calls_per_month  INTEGER CHECK (max_calls_per_month >= 0),
    history_hours        INTEGER NOT NULL CHECK (history_hours > 0),
    sort_order           INTEGER NOT NULL DEFAULT 0,
    is_default           BOOLEAN NOT NULL DEFAULT false,
    -- An inactive plan is not offered any more. Users on it keep its limits.
    active               BOOLEAN NOT NULL DEFAULT true
);

-- Exactly one plan is the default. A user without a paid subscription has it.
CREATE UNIQUE INDEX plans_one_default_idx ON plans (is_default) WHERE is_default;

INSERT INTO plans (code, name, price_cents, currency, interval, stripe_lookup_key, max_servers,
                   max_tools_per_server, max_calls_per_month, history_hours, sort_order, is_default)
VALUES ('free', 'Free', 0, 'usd', NULL, NULL, 1, 3, 100, 24, 0, true),
       ('pro', 'Pro', 900, 'usd', 'month', 'toolport_pro_monthly', 10, NULL, 10000, 720, 1, false);

-- The Stripe webhook is the only writer of these columns. The subscription columns are for
-- display and for the quota period. `plan_id` alone decides the limits.
ALTER TABLE users
    ADD COLUMN plan_id                BIGINT REFERENCES plans (id),
    ADD COLUMN stripe_customer_id     TEXT UNIQUE,
    ADD COLUMN stripe_subscription_id TEXT,
    ADD COLUMN subscription_status    TEXT,
    ADD COLUMN current_period_start   TIMESTAMPTZ,
    ADD COLUMN current_period_end     TIMESTAMPTZ,
    ADD COLUMN cancel_at_period_end   BOOLEAN NOT NULL DEFAULT false;

UPDATE users SET plan_id = (SELECT id FROM plans WHERE is_default);
ALTER TABLE users ALTER COLUMN plan_id SET NOT NULL;

-- A column default cannot be a subquery, so a trigger gives new users the default plan.
CREATE FUNCTION users_default_plan() RETURNS trigger AS $$
BEGIN
    IF NEW.plan_id IS NULL THEN
        NEW.plan_id := (SELECT id FROM plans WHERE is_default);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER users_default_plan BEFORE INSERT ON users
    FOR EACH ROW EXECUTE FUNCTION users_default_plan();

-- Tool calls per user and quota period. The cleanup deletes old rows of `calls`, so the
-- quota cannot count from there.
CREATE TABLE usage (
    user_id      BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    period_start TIMESTAMPTZ NOT NULL,
    calls        INTEGER NOT NULL DEFAULT 0 CHECK (calls >= 0),
    PRIMARY KEY (user_id, period_start)
);
