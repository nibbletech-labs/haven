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
--
-- Schema: all Haven objects live in a dedicated `haven` schema (not `public`),
-- so Haven can co-tenant a shared Supabase project alongside other apps. Every
-- table/policy/index/FK/RPC is `haven.`-qualified; haven-sync sends PostgREST
-- the `Accept-Profile`/`Content-Profile: haven` headers to route to it. The
-- Storage bucket and its policies stay schema-agnostic (in the `storage`
-- schema, keyed by bucket_id + path).

create schema if not exists haven;

-- ============================================================
-- Projects
-- ============================================================
create table haven.projects (
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
create table haven.nodes (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  project_id   uuid not null references haven.projects(public_id) on delete cascade,
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
create index nodes_project_idx on haven.nodes(project_id);
create index nodes_status_idx on haven.nodes(project_id, status);

-- ============================================================
-- Structural edges (mutable). UUID FKs to nodes(public_id).
-- ============================================================
create table haven.decomposition_edges (
  user_id     text not null default (auth.jwt() ->> 'sub'),
  parent_id   uuid not null references haven.nodes(public_id) on delete cascade,
  child_id    uuid not null references haven.nodes(public_id) on delete cascade,
  created_at  timestamptz not null default now(),
  client_id   text not null,
  primary key (parent_id, child_id),
  check (parent_id <> child_id),
  unique (user_id, client_id)
);

create table haven.dependency_edges (
  user_id        text not null default (auth.jwt() ->> 'sub'),
  node_id        uuid not null references haven.nodes(public_id) on delete cascade,
  depends_on_id  uuid not null references haven.nodes(public_id) on delete cascade,
  created_at     timestamptz not null default now(),
  client_id      text not null,
  primary key (node_id, depends_on_id),
  check (node_id <> depends_on_id),
  unique (user_id, client_id)
);

create table haven.grouping_edges (
  user_id     text not null default (auth.jwt() ->> 'sub'),
  group_id    uuid not null references haven.nodes(public_id) on delete cascade,
  member_id   uuid not null references haven.nodes(public_id) on delete cascade,
  created_at  timestamptz not null default now(),
  client_id   text not null,
  primary key (group_id, member_id),
  check (group_id <> member_id),
  unique (user_id, client_id)
);

-- ============================================================
-- Lineage (append-only core)
-- ============================================================
create table haven.lineage_events (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  project_id   uuid not null references haven.projects(public_id) on delete cascade,
  event_type   text not null
    check (event_type in ('split','merge','supersede','update','archive','reopen')),
  rationale    text,
  triggered_by text,
  context      jsonb not null default '{}',
  created_at   timestamptz not null default now(),
  client_id    text not null,
  unique (user_id, client_id)
);

create table haven.lineage_edges (
  user_id      text not null default (auth.jwt() ->> 'sub'),
  event_id     uuid not null references haven.lineage_events(public_id) on delete cascade,
  from_node_id uuid not null references haven.nodes(public_id),
  to_node_id   uuid not null references haven.nodes(public_id),
  primary key (event_id, from_node_id, to_node_id)
);
create index lineage_from_idx on haven.lineage_edges(from_node_id);
create index lineage_to_idx on haven.lineage_edges(to_node_id);

-- ============================================================
-- Artifacts (mutable; file blobs live in Storage)
-- ============================================================
create table haven.artifacts (
  public_id    uuid primary key,
  user_id      text not null default (auth.jwt() ->> 'sub'),
  node_id      uuid not null references haven.nodes(public_id) on delete cascade,
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
create index artifacts_node_idx on haven.artifacts(node_id);

-- ============================================================
-- Row-Level Security — every table scoped to the Auth0 subject.
-- ============================================================
alter table haven.projects            enable row level security;
alter table haven.nodes               enable row level security;
alter table haven.decomposition_edges enable row level security;
alter table haven.dependency_edges    enable row level security;
alter table haven.grouping_edges      enable row level security;
alter table haven.lineage_events      enable row level security;
alter table haven.lineage_edges       enable row level security;
alter table haven.artifacts           enable row level security;

-- Mutable tables: full owner access (select/insert/update/delete).
create policy projects_owner on haven.projects
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy nodes_owner on haven.nodes
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy decomposition_owner on haven.decomposition_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy dependency_owner on haven.dependency_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy grouping_owner on haven.grouping_edges
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);
create policy artifacts_owner on haven.artifacts
  using ((auth.jwt() ->> 'sub') = user_id)
  with check ((auth.jwt() ->> 'sub') = user_id);

-- Append-only core: SELECT + INSERT only. No UPDATE/DELETE policies exist, so
-- those operations are denied to clients (the immutable log, SPEC §5).
create policy lineage_events_select on haven.lineage_events
  for select using ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_events_insert on haven.lineage_events
  for insert with check ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_edges_select on haven.lineage_edges
  for select using ((auth.jwt() ->> 'sub') = user_id);
create policy lineage_edges_insert on haven.lineage_edges
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
-- The function lives in the `haven` schema; its DELETEs are schema-qualified so
-- they resolve correctly regardless of the SECURITY DEFINER search_path. We pin
-- `search_path = haven, public` anyway (haven for the qualified targets, public
-- left available; the storage call is fully qualified too).
create or replace function haven.delete_my_account()
returns void
language plpgsql
security definer
set search_path = haven, public
as $$
declare
  uid text := auth.jwt() ->> 'sub';
begin
  if uid is null then
    raise exception 'no authenticated user';
  end if;
  -- Children first where FKs aren't ON DELETE CASCADE from the owner row.
  delete from haven.lineage_edges where user_id = uid;
  delete from haven.lineage_events where user_id = uid;
  delete from haven.artifacts where user_id = uid;
  delete from haven.decomposition_edges where user_id = uid;
  delete from haven.dependency_edges where user_id = uid;
  delete from haven.grouping_edges where user_id = uid;
  delete from haven.nodes where user_id = uid;
  delete from haven.projects where user_id = uid;
  delete from storage.objects
    where bucket_id = 'haven-content' and (storage.foldername(name))[1] = uid;
end;
$$;

-- ============================================================
-- GRANTs — make the `haven` schema reachable by the Data API roles. RLS still
-- gates which rows each user sees; these GRANTs only let the role reach the
-- tables/routines/sequences at all. USAGE on the schema is the easily-forgotten
-- one. ALTER DEFAULT PRIVILEGES covers objects added by later migrations.
-- ============================================================
grant usage on schema haven to anon, authenticated, service_role;
grant all on all tables in schema haven to anon, authenticated, service_role;
grant all on all routines in schema haven to anon, authenticated, service_role;
grant all on all sequences in schema haven to anon, authenticated, service_role;
alter default privileges for role postgres in schema haven grant all on tables to anon, authenticated, service_role;
alter default privileges for role postgres in schema haven grant all on routines to anon, authenticated, service_role;
alter default privileges for role postgres in schema haven grant all on sequences to anon, authenticated, service_role;
