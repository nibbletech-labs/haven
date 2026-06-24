-- 0007_project_lifecycle.sql — remote mirror of local 008: project lifecycle
-- columns (in-place ALTER, no table rebuild — symmetric with the local migration).
alter table haven.projects
  add column status text not null default 'active'
    check (status in ('active','archived')),
  add column archived_at timestamptz,
  add column archived_reason text,
  add column deleted_at timestamptz;

create index idx_projects_status on haven.projects(status);
