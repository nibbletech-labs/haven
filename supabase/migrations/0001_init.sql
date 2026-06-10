-- Haven remote schema (SPEC §5). Mirrors the local SQLite structure with:
--   * UUID primary keys (= the local `public_id`); local integer ids never sync.
--   * a `user_id` stamp defaulted from the Auth0 `sub` claim (Supabase
--     Third-Party Auth), and RLS on every table scoped to it.
--   * the append-only lineage core given INSERT-only access (no UPDATE/DELETE).
--   * edges carrying `user_id` so RLS applies, and UUID foreign keys.
--
-- Auth: Supabase is configured to trust Auth0 as a third-party issuer; it
-- verifies incoming Auth0 JWTs against Auth0's JWKS and exposes the claims via
-- auth.jwt(). We read the subject with (auth.jwt() ->> 'sub').

-- ============================================================
-- Projects
-- ============================================================
create table projects (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  key          text not null,
  ref_prefix   text not null,
  ref_counter  integer not null default 0,
  title        text not null,
  description  text,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  client_id    text not null,
  revision     integer not null default 1,
  unique (user_id, key),
  unique (user_id, client_id)
);

-- ============================================================
-- Nodes
-- ============================================================
create table nodes (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  project_id   uuid not null references projects(public_id) on delete cascade,
  ref          text not null,
  title        text not null,
  body         text,
  done_looks_like text,                        -- acceptance statement (verify anchor)
  why          text,                            -- one-line provenance / vision trace
  type         text not null default 'task'
    check (type in ('task','code','research','data','design','admin','release','phase','gate','anchor')),
  status       text not null default 'discovery'
    check (status in ('discovery','definition','ready','in_progress','blocked','done','superseded','archived')),
  owner_kind   text check (owner_kind in ('human','ai')),
  assignee     text,
  wait_state   text check (wait_state in ('on_human','on_dependency','on_external')),
  committed    boolean not null default false,
  priority     integer check (priority between 0 and 4),
  sort_key     text,
  metadata     jsonb not null default '{}',
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  archived_at  timestamptz,
  client_id    text not null,
  revision     integer not null default 1,
  unique (user_id, project_id, ref),
  unique (user_id, client_id)
);
create index nodes_project_idx on nodes(project_id);
create index nodes_status_idx on nodes(project_id, status);

-- ============================================================
-- Structural edges (mutable). UUID FKs to nodes(public_id).
-- ============================================================
create table decomposition_edges (
  user_id     text not null default (auth.jwt() ->> 'sub'),
  parent_id   uuid not null references nodes(public_id) on delete cascade,
  child_id    uuid not null references nodes(public_id) on delete cascade,
  created_at  timestamptz not null default now(),
  client_id   text not null,
  primary key (parent_id, child_id),
  check (parent_id <> child_id),
  unique (user_id, client_id)
);

create table dependency_edges (
  user_id        text not null default (auth.jwt() ->> 'sub'),
  node_id        uuid not null references nodes(public_id) on delete cascade,
  depends_on_id  uuid not null references nodes(public_id) on delete cascade,
  created_at     timestamptz not null default now(),
  client_id      text not null,
  primary key (node_id, depends_on_id),
  check (node_id <> depends_on_id),
  unique (user_id, client_id)
);

create table grouping_edges (
  user_id     text not null default (auth.jwt() ->> 'sub'),
  group_id    uuid not null references nodes(public_id) on delete cascade,
  member_id   uuid not null references nodes(public_id) on delete cascade,
  created_at  timestamptz not null default now(),
  client_id   text not null,
  primary key (group_id, member_id),
  check (group_id <> member_id),
  unique (user_id, client_id)
);

-- ============================================================
-- Lineage (append-only core)
-- ============================================================
create table lineage_events (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  project_id   uuid not null references projects(public_id) on delete cascade,
  event_type   text not null
    check (event_type in ('split','merge','supersede','update','archive','reopen')),
  rationale    text,
  triggered_by text,
  context      jsonb not null default '{}',
  created_at   timestamptz not null default now(),
  client_id    text not null,
  unique (user_id, client_id)
);

create table lineage_edges (
  user_id      text not null default (auth.jwt() ->> 'sub'),
  event_id     uuid not null references lineage_events(public_id) on delete cascade,
  from_node_id uuid not null references nodes(public_id),
  to_node_id   uuid not null references nodes(public_id),
  primary key (event_id, from_node_id, to_node_id)
);
create index lineage_from_idx on lineage_edges(from_node_id);
create index lineage_to_idx on lineage_edges(to_node_id);

-- ============================================================
-- Artifacts (mutable; file blobs live in Storage)
-- ============================================================
create table artifacts (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  node_id      uuid not null references nodes(public_id) on delete cascade,
  role         text not null
    check (role in ('spec','research','design','handoff','decision','scratch','source','delivery','vision')),
  kind         text not null default 'file' check (kind in ('file','external','delivery')),
  path         text,
  uri          text,
  title        text,
  excerpt      text,
  from_owner   text check (from_owner in ('human','ai')),
  to_owner     text check (to_owner in ('human','ai')),
  content_hash text,
  remote_path  text,
  metadata     jsonb not null default '{}',
  created_at   timestamptz not null default now(),
  created_by   text,
  client_id    text not null,
  revision     integer not null default 1,
  unique (user_id, client_id)
);
create index artifacts_node_idx on artifacts(node_id);

-- ============================================================
-- Row-Level Security — every table scoped to the Auth0 subject.
-- ============================================================
alter table projects            enable row level security;
alter table nodes               enable row level security;
alter table decomposition_edges enable row level security;
alter table dependency_edges    enable row level security;
alter table grouping_edges      enable row level security;
alter table lineage_events      enable row level security;
alter table lineage_edges       enable row level security;
alter table artifacts           enable row level security;

-- Mutable tables: full owner access (select/insert/update/delete).
create policy projects_owner on projects
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy nodes_owner on nodes
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy decomposition_owner on decomposition_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy dependency_owner on dependency_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy grouping_owner on grouping_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy artifacts_owner on artifacts
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);

-- Append-only core: SELECT + INSERT only. No UPDATE/DELETE policies exist, so
-- those operations are denied to clients (the immutable log, SPEC §5).
create policy lineage_events_select on lineage_events
  for select using ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_events_insert on lineage_events
  for insert with check ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_edges_select on lineage_edges
  for select using ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_edges_insert on lineage_edges
  for insert with check ((auth.jwt() ->> 'sub') = user_id);

-- ============================================================
-- Storage: one private bucket; object key is
--   <user_id>/<project-key>/items/<ref>/<artifact>
-- and the first path segment must equal the Auth0 subject.
-- ============================================================
insert into storage.buckets (id, name, public)
values ('haven-content', 'haven-content', false)
on conflict (id) do nothing;

create policy haven_content_read on storage.objects
  for select using (
    bucket_id = 'haven-content'
    and (storage.foldername(name))[1] = (auth.jwt() ->> 'sub')
  );
create policy haven_content_write on storage.objects
  for insert with check (
    bucket_id = 'haven-content'
    and (storage.foldername(name))[1] = (auth.jwt() ->> 'sub')
  );
create policy haven_content_update on storage.objects
  for update using (
    bucket_id = 'haven-content'
    and (storage.foldername(name))[1] = (auth.jwt() ->> 'sub')
  );
create policy haven_content_delete on storage.objects
  for delete using (
    bucket_id = 'haven-content'
    and (storage.foldername(name))[1] = (auth.jwt() ->> 'sub')
  );

-- ============================================================
-- Account deletion: append-only rows can't be client-deleted, so a
-- SECURITY DEFINER RPC cascades everything owned by the caller.
-- ============================================================
create or replace function delete_my_account()
returns void
language plpgsql
security definer
set search_path = public
as $$
declare
  uid text := auth.jwt() ->> 'sub';
begin
  if uid is null then
    raise exception 'no authenticated user';
  end if;
  -- Children first where FKs aren't ON DELETE CASCADE from the owner row.
  delete from lineage_edges where user_id = uid;
  delete from lineage_events where user_id = uid;
  delete from artifacts where user_id = uid;
  delete from decomposition_edges where user_id = uid;
  delete from dependency_edges where user_id = uid;
  delete from grouping_edges where user_id = uid;
  delete from nodes where user_id = uid;
  delete from projects where user_id = uid;
  delete from storage.objects
    where bucket_id = 'haven-content' and (storage.foldername(name))[1] = uid;
end;
$$;
