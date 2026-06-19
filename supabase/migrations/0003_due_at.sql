-- HV-67: nullable calendar-date deadline on nodes (remote mirror).
-- Postgres adds the column in place (no table rebuild). The wire JSON carries
-- `due_at` as a `YYYY-MM-DD` string; a Postgres `date` column accepts it. The
-- local side stores the same string as TEXT. Validation lives at the Store
-- boundary, not as a DB constraint.
alter table nodes add column due_at date;
