-- Users sign in with GitHub only, so the GitHub account id is the identity.
CREATE TABLE users (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    github_id    BIGINT NOT NULL UNIQUE,
    github_login TEXT NOT NULL,
    name         TEXT,
    avatar_url   TEXT,
    theme        TEXT NOT NULL DEFAULT 'neon' CHECK (theme IN ('neon', 'calm', 'light')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
