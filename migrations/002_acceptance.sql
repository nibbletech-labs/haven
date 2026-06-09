-- 002: acceptance + provenance fields on nodes.
--
-- `done_looks_like` is the acceptance statement a dispatcher (e.g. builder's
-- `orchestrate`) verifies an item's output against — the anchor for the
-- ready→done transition. `why` is a one-line vision/provenance trace.
--
-- Both are short, queryable, structured fields that drive routing and
-- verification, so they live on the node row rather than buried in `body` or a
-- spec artifact. Nullable: present only when known. (Added as a separate
-- migration, not folded into 001, so existing v1 databases pick them up.)

ALTER TABLE nodes ADD COLUMN done_looks_like TEXT;
ALTER TABLE nodes ADD COLUMN why TEXT;
