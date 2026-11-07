-- The queue's agent no longer fixes what a review sends back. A review held for that fix posted
-- nothing yet: it runs again, and posts to the author.
UPDATE agent_reviews SET status = 'pending', fixing = false, claim = claim + 1, claimed_at = NULL
WHERE fixing;
