-- Host machines running session manager agents.
-- Each host can serve multiple VDI sessions.
CREATE TABLE IF NOT EXISTS hosts (
    id          UUID        PRIMARY KEY,
    url         TEXT        NOT NULL,       -- session manager API URL, e.g. http://192.168.1.10:9100
    label       TEXT,                       -- friendly name, e.g. "win-server-01"
    platform    TEXT        NOT NULL DEFAULT 'windows',  -- windows, linux, macos
    healthy     BOOLEAN     NOT NULL DEFAULT true,
    last_seen   TIMESTAMPTZ,
    max_sessions INTEGER    NOT NULL DEFAULT 5,  -- max concurrent VDI sessions
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Which users are allowed to use which hosts.
-- An admin assigns users to hosts; the gateway creates sessions on those hosts.
CREATE TABLE IF NOT EXISTS user_host_assignments (
    user_sub    TEXT        NOT NULL,
    host_id     UUID        NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    priority    INTEGER     NOT NULL DEFAULT 0,  -- higher = preferred
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_sub, host_id)
);

-- Active VDI sessions managed by the gateway.
-- Created when a user connects; destroyed on disconnect or admin action.
CREATE TABLE IF NOT EXISTS vdi_sessions (
    id              TEXT        PRIMARY KEY,  -- session UUID from session manager
    user_sub        TEXT        NOT NULL,
    user_email      TEXT,
    host_id         UUID        NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    backend_port    INTEGER     NOT NULL,
    display_index   INTEGER,
    os_user         TEXT,
    status          TEXT        NOT NULL DEFAULT 'active',  -- active, disconnected, terminated
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_activity   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_vdi_sessions_user ON vdi_sessions(user_sub);
CREATE INDEX IF NOT EXISTS idx_vdi_sessions_host ON vdi_sessions(host_id);
