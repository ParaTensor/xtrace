-- Add index on parent_observation_id in observations table to optimize parent-child trace tree traversal.
CREATE INDEX IF NOT EXISTS idx_observations_parent_observation_id
ON observations (parent_observation_id)
WHERE parent_observation_id IS NOT NULL;
