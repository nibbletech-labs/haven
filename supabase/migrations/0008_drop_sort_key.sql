-- HV-319: retire manual fine rank (local counterpart: 009_drop_sort_key).
-- Upgrade clients before applying: old clients still send this retired field.
alter table haven.nodes drop column sort_key;
