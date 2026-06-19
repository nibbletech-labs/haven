-- HV-125: REMOVE the `owner_eligible` eligibility axis (remote mirror; reverts
-- 0004_owner_eligible). Readiness is already ready + committed + done_looks_like —
-- a different dimension from ownership; `owner_eligible` conflated them and added a
-- redundant auto-pull gate, so `next --owner` reverts to filtering `owner_kind`.
-- Postgres drops a CHECK-constrained column in place (no table rebuild — unlike
-- SQLite, cf. the local 006_drop_owner_eligible.sql).
alter table nodes drop column owner_eligible;
