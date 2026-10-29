-- A repo queues when its default branch has `.mergepatrol/mergepatrol.yml`. `enabled` overrides
-- that from the dashboard: NULL follows the file.
ALTER TABLE repos ALTER COLUMN enabled DROP NOT NULL;
-- Off was the default before the file: turning a repo off only went back to it.
UPDATE repos SET enabled = NULL WHERE NOT enabled;
ALTER TABLE repos ADD COLUMN has_config BOOLEAN NOT NULL DEFAULT false;
-- Why the queue is paused: the config file does not parse.
ALTER TABLE repos ADD COLUMN config_error TEXT;
