-- Add VM metadata to backends table
ALTER TABLE backends
  ADD COLUMN IF NOT EXISTS vm_type   TEXT,
  ADD COLUMN IF NOT EXISTS vm_name   TEXT,
  ADD COLUMN IF NOT EXISTS vm_ns     TEXT,
  ADD COLUMN IF NOT EXISTS os_type   TEXT,
  ADD COLUMN IF NOT EXISTS disk_pvc  TEXT;

-- Track VM power state separately from health
CREATE TABLE IF NOT EXISTS vm_states (
  backend_id   UUID        PRIMARY KEY REFERENCES backends(id) ON DELETE CASCADE,
  state        TEXT        NOT NULL DEFAULT 'stopped',
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
