-- Stripe can send the same webhook event more than once. The handler writes the event id in
-- the same transaction as the plan change, so it applies each event only once.
CREATE TABLE stripe_events (
    id           TEXT PRIMARY KEY,
    type         TEXT NOT NULL,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
