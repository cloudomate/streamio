CREATE TABLE IF NOT EXISTS known_users (
    sub TEXT PRIMARY KEY,
    email TEXT,
    display_name TEXT,
    last_login TIMESTAMPTZ DEFAULT now()
)

