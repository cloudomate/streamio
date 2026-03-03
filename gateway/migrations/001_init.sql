CREATE TABLE IF NOT EXISTS backends (
    id          UUID        PRIMARY KEY,
    url         TEXT        NOT NULL,
    label       TEXT,
    healthy     BOOLEAN     NOT NULL DEFAULT true,
    last_seen   TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS assignments (
    user_sub    TEXT        PRIMARY KEY,
    backend_id  UUID        NOT NULL REFERENCES backends(id) ON DELETE CASCADE,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
