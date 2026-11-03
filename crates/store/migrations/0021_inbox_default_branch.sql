-- Each PR's node now has its repo's default branch, for the queue page's open PRs. Until a
-- repo's next full read gives every node one, the page reads GitHub.
UPDATE inbox_repos SET indexed_at = NULL;
