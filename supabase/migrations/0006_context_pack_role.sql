-- HV-124: promote `context-pack` to a first-class artifact role (remote mirror of
-- the local 007_context_pack_role.sql). Postgres alters a CHECK constraint in place
-- (drop + re-add) — no table rebuild, unlike SQLite. Then reclassify the existing
-- magic-filename packs (role='spec' + the canonical context-pack.md path) to the
-- new role, mirroring the local UPDATE.
alter table haven.artifacts drop constraint if exists artifacts_role_check;
alter table haven.artifacts add constraint artifacts_role_check
  check (role in ('spec','research','design','handoff','decision',
                  'scratch','source','delivery','vision','context-pack'));

update haven.artifacts
   set role = 'context-pack'
 where role = 'spec' and path like '%/context-pack.md';
