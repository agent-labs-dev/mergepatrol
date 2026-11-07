-- Where the config file the queue last read is: `.mergeloop/mergeloop.yml`, or the path repos
-- set up before the rename keep. NULL for a read made before this, which only that path had.
ALTER TABLE github_repos ADD COLUMN config_path TEXT;
