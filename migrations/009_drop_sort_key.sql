-- HV-319: retire manual fine rank; priority and creation time order work.
-- This column has no constraints/indexes; DROP preserves node IDs, FTS and edges.
ALTER TABLE nodes DROP COLUMN sort_key;
