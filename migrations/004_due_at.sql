-- HV-67: nullable calendar-date deadline on nodes.
-- Stored as `YYYY-MM-DD` text (sortable lexically, same convention as the
-- existing `*_at` text timestamps); validated at the Store boundary, never by a
-- DB CHECK. A light additive ALTER — no table rebuild (cf. 002_acceptance.sql).
ALTER TABLE nodes ADD COLUMN due_at TEXT;
