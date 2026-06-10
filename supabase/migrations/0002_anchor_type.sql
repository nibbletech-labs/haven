-- Add the living-doc anchor node type to the remote mirror.

alter table nodes drop constraint nodes_type_check;
alter table nodes add constraint nodes_type_check
  check (type in ('task','code','research','data','design','admin','release','phase','gate','anchor'));
