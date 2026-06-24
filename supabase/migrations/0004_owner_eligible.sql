-- HV-66: eligibility-vs-assignment axis on nodes (remote mirror).
-- Postgres adds a CHECK-constrained column in place (no table rebuild — unlike
-- SQLite, cf. the local 005_owner_eligible.sql). `owner_eligible` is a NEW,
-- additive axis DISTINCT from `owner_kind`; NULL = untriaged, no backfill. The
-- wire JSON carries it as a string; a Postgres `text` column with the CHECK
-- accepts it. Validation otherwise lives at the local Store boundary.
alter table haven.nodes add column owner_eligible text
  check (owner_eligible in ('human','ai','any'));
