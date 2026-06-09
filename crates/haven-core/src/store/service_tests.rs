//! End-to-end service-layer tests over an in-memory store: item lifecycle, the
//! two axes, the four edge layers (+ cycle guards), evolve/lineage, `next`,
//! search, the resolver, and rank ordering.

use super::*;
use crate::model::*;

fn store() -> Store {
    let s = Store::open_in_memory().unwrap();
    s.add_project("haven", Some("HV"), "Haven", None).unwrap();
    s.use_project("haven").unwrap();
    s
}

fn add(s: &Store, title: &str) -> Item {
    s.add_item(
        None,
        NewItem {
            title: title.into(),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn add_mints_sequential_refs_and_defaults() {
    let s = store();
    let a = add(&s, "First");
    let b = add(&s, "Second");
    assert_eq!(a.reference, "HV-1");
    assert_eq!(b.reference, "HV-2");
    // Default node is floating, uncommitted, discovery, unowned.
    assert_eq!(a.status, Status::Discovery);
    assert!(!a.committed);
    assert_eq!(a.priority, None);
    assert!(a.owner_kind.is_none());
    assert_eq!(a.node_type, NodeType::Task);
}

#[test]
fn done_looks_like_and_why_round_trip() {
    let s = store();
    // Set acceptance + provenance on create.
    let item = s
        .add_item(
            None,
            NewItem {
                title: "Cache the JWKS lookup".into(),
                done_looks_like: Some("p95 verify < 5ms; refresh on kid-miss".into()),
                why: Some("auth latency goal from perf review".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        item.done_looks_like.as_deref(),
        Some("p95 verify < 5ms; refresh on kid-miss")
    );
    assert_eq!(
        item.why.as_deref(),
        Some("auth latency goal from perf review")
    );

    // Re-read confirms they persisted (not just echoed).
    let fetched = s.get_item(None, &item.reference, &[]).unwrap();
    assert_eq!(fetched.done_looks_like, item.done_looks_like);

    // Update revises acceptance; `why` is left untouched.
    let updated = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                done_looks_like: Some("p95 verify < 3ms".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.done_looks_like.as_deref(), Some("p95 verify < 3ms"));
    assert_eq!(
        updated.why.as_deref(),
        Some("auth latency goal from perf review")
    );

    // A bare item has neither (the fields stay absent from JSON).
    let bare = add(&s, "Floating idea");
    assert!(bare.done_looks_like.is_none());
    assert!(serde_json::to_value(&bare)
        .unwrap()
        .get("done_looks_like")
        .is_none());
}

#[test]
fn add_with_axes_and_edges() {
    let s = store();
    let parent = add(&s, "Parent");
    let dep = add(&s, "Prereq");
    let item = s
        .add_item(
            None,
            NewItem {
                title: "Child".into(),
                status: Some(Status::Ready),
                priority: Some(1),
                commit: true,
                assign: Some(OwnerKind::Ai),
                parent: Some(parent.reference.clone()),
                depends_on: Some(dep.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(item.committed);
    assert_eq!(item.priority, Some(1));
    assert_eq!(item.owner_kind, Some(OwnerKind::Ai));

    let full = s
        .get_item(None, &item.reference, &[Include::Edges])
        .unwrap();
    let edges = full.edges.unwrap();
    assert_eq!(edges.parents, vec![parent.reference.clone()]);
    assert_eq!(edges.depends_on, vec![dep.reference.clone()]);
}

#[test]
fn ref_or_public_id_both_resolve() {
    let s = store();
    let a = add(&s, "Thing");
    let by_ref = s.get_item(None, &a.reference, &[]).unwrap();
    let by_uuid = s.get_item(None, &a.public_id, &[]).unwrap();
    assert_eq!(by_ref.public_id, by_uuid.public_id);
}

#[test]
fn update_commit_assign_and_wait() {
    let s = store();
    let a = add(&s, "Work");
    let r = &a.reference;

    let updated = s
        .update_item(
            None,
            r,
            ItemUpdate {
                status: Some(Status::Blocked),
                wait: Some(WaitUpdate::Set(WaitState::OnExternal)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.status, Status::Blocked);
    assert_eq!(updated.wait_state, Some(WaitState::OnExternal));
    assert!(updated.revision > a.revision);

    let cleared = s
        .update_item(
            None,
            r,
            ItemUpdate {
                wait: Some(WaitUpdate::Clear),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(cleared.wait_state, None);

    let committed = s.commit_item(None, r, Some(0)).unwrap();
    assert!(committed.committed);
    assert_eq!(committed.priority, Some(0));
    let iced = s.uncommit_item(None, r).unwrap();
    assert!(!iced.committed);
    assert_eq!(iced.priority, Some(0), "uncommit retains the priority band");

    let assigned = s
        .assign_item(None, r, OwnerKind::Human, Some("human:tom"))
        .unwrap();
    assert_eq!(assigned.owner_kind, Some(OwnerKind::Human));
    assert_eq!(assigned.assignee.as_deref(), Some("human:tom"));
}

#[test]
fn next_respects_ready_committed_wait_and_dependencies() {
    let s = store();
    // Not ready -> excluded.
    let _discovery = s
        .add_item(
            None,
            NewItem {
                title: "fuzzy".into(),
                commit: true,
                status: Some(Status::Discovery),
                ..Default::default()
            },
        )
        .unwrap();
    // Ready but uncommitted -> excluded.
    let _floating = s
        .add_item(
            None,
            NewItem {
                title: "parked".into(),
                status: Some(Status::Ready),
                ..Default::default()
            },
        )
        .unwrap();
    // Ready + committed + waiting -> excluded.
    let waiting = s
        .add_item(
            None,
            NewItem {
                title: "waiting".into(),
                status: Some(Status::Ready),
                commit: true,
                ..Default::default()
            },
        )
        .unwrap();
    s.update_item(
        None,
        &waiting.reference,
        ItemUpdate {
            wait: Some(WaitUpdate::Set(WaitState::OnHuman)),
            ..Default::default()
        },
    )
    .unwrap();

    // Ready + committed + blocked by an unfinished dependency -> excluded.
    let prereq = add(&s, "prereq");
    let blocked = s
        .add_item(
            None,
            NewItem {
                title: "blocked".into(),
                status: Some(Status::Ready),
                commit: true,
                depends_on: Some(prereq.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    // The one truly dispatchable item.
    let go = s
        .add_item(
            None,
            NewItem {
                title: "go".into(),
                status: Some(Status::Ready),
                commit: true,
                priority: Some(0),
                ..Default::default()
            },
        )
        .unwrap();

    let next = s.next(None, None, None).unwrap();
    let refs: Vec<&str> = next.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(
        refs,
        vec![go.reference.as_str()],
        "only the unblocked ready item dispatches"
    );

    // Finishing the prereq unblocks the dependent item.
    s.update_item(
        None,
        &prereq.reference,
        ItemUpdate {
            status: Some(Status::Done),
            ..Default::default()
        },
    )
    .unwrap();
    let next = s.next(None, None, None).unwrap();
    let refs: Vec<String> = next.iter().map(|i| i.reference.clone()).collect();
    assert!(refs.contains(&blocked.reference));
    assert!(refs.contains(&go.reference));
    // Priority 0 (go) sorts before the unprioritised blocked item.
    assert_eq!(refs[0], go.reference);
}

#[test]
fn next_owner_filter() {
    let s = store();
    let ai = s
        .add_item(
            None,
            NewItem {
                title: "ai work".into(),
                status: Some(Status::Ready),
                commit: true,
                assign: Some(OwnerKind::Ai),
                ..Default::default()
            },
        )
        .unwrap();
    let _human = s
        .add_item(
            None,
            NewItem {
                title: "human work".into(),
                status: Some(Status::Ready),
                commit: true,
                assign: Some(OwnerKind::Human),
                ..Default::default()
            },
        )
        .unwrap();
    let next = s.next(None, Some(OwnerKind::Ai), None).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].reference, ai.reference);
}

#[test]
fn decomposition_cycle_is_rejected() {
    let s = store();
    let a = add(&s, "A");
    let b = add(&s, "B");
    s.decompose(None, &a.reference, &b.reference, false)
        .unwrap();
    // b -> a would close a cycle.
    let err = s
        .decompose(None, &b.reference, &a.reference, false)
        .unwrap_err();
    assert_eq!(err.code(), "graph_rule");
    // Idempotent re-add of an existing edge is fine.
    s.decompose(None, &a.reference, &b.reference, false)
        .unwrap();
}

#[test]
fn dependency_cycle_is_rejected() {
    let s = store();
    let a = add(&s, "A");
    let b = add(&s, "B");
    s.depend(None, &a.reference, &b.reference, false).unwrap(); // a depends on b
    let err = s
        .depend(None, &b.reference, &a.reference, false)
        .unwrap_err(); // b depends on a
    assert_eq!(err.code(), "graph_rule");
}

#[test]
fn grouping_requires_container_node() {
    let s = store();
    let task = add(&s, "a task");
    let member = add(&s, "member");
    // A plain task cannot be a group target.
    assert!(s
        .group(None, &task.reference, &member.reference, false)
        .is_err());

    let release = s
        .add_item(
            None,
            NewItem {
                title: "v1".into(),
                node_type: Some(NodeType::Release),
                ..Default::default()
            },
        )
        .unwrap();
    s.group(None, &release.reference, &member.reference, false)
        .unwrap();
    let full = s
        .get_item(None, &member.reference, &[Include::Edges])
        .unwrap();
    assert_eq!(full.edges.unwrap().groups, vec![release.reference.clone()]);

    // list --group filters to members.
    let listed = s
        .list_items(
            None,
            &ItemFilter {
                group: Some(release.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].reference, member.reference);

    s.group(None, &release.reference, &member.reference, true)
        .unwrap();
    let after = s
        .get_item(None, &member.reference, &[Include::Edges])
        .unwrap();
    assert!(after.edges.unwrap().groups.is_empty());
}

#[test]
fn split_records_lineage_and_supersedes_source() {
    let s = store();
    let big = add(&s, "Big epic");
    let res = s
        .evolve_split(
            None,
            &big.reference,
            &["Part A".into(), "Part B".into()],
            Some("too big"),
            Some("human:tom"),
        )
        .unwrap();
    assert_eq!(res.new.len(), 2);
    assert_eq!(res.superseded, vec![big.reference.clone()]);

    let source = s
        .get_item(None, &big.reference, &[Include::Lineage])
        .unwrap();
    assert_eq!(source.status, Status::Superseded);
    let lineage = source.lineage.unwrap();
    assert_eq!(lineage.len(), 1);
    assert_eq!(lineage[0].event_type, EventType::Split);
    assert_eq!(lineage[0].from, vec![big.reference.clone()]);
    assert_eq!(lineage[0].to.len(), 2);

    // Resolver follows lineage from the superseded source to its live parts.
    let live = s.resolve_live(None, &big.reference).unwrap();
    let live_refs: Vec<&str> = live.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(live_refs.len(), 2);
    for part in &res.new {
        assert!(live_refs.contains(&part.reference.as_str()));
    }
}

#[test]
fn merge_and_supersede() {
    let s = store();
    let a = add(&s, "Auth backend");
    let b = add(&s, "Auth frontend");
    let res = s
        .evolve_merge(
            None,
            &[a.reference.clone(), b.reference.clone()],
            "Unified auth",
            Some("same problem"),
            None,
        )
        .unwrap();
    assert_eq!(res.new.len(), 1);
    assert_eq!(res.superseded.len(), 2);
    assert_eq!(
        s.get_item(None, &a.reference, &[]).unwrap().status,
        Status::Superseded
    );

    let merged = &res.new[0];
    let replacement = add(&s, "Even better auth");
    let sup = s
        .evolve_supersede(
            None,
            &merged.reference,
            &replacement.reference,
            Some("redesign"),
            None,
        )
        .unwrap();
    assert_eq!(sup.superseded, vec![merged.reference.clone()]);
    let live = s.resolve_live(None, &merged.reference).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].reference, replacement.reference);
}

#[test]
fn archive_and_reopen_emit_lineage() {
    let s = store();
    let a = add(&s, "Maybe later");
    s.archive_item(None, &a.reference, Some("won't fix"), Some("human:tom"))
        .unwrap();
    let archived = s.get_item(None, &a.reference, &[Include::Lineage]).unwrap();
    assert_eq!(archived.status, Status::Archived);
    assert!(archived.archived_at.is_some());
    assert_eq!(
        archived.lineage.as_ref().unwrap()[0].event_type,
        EventType::Archive
    );

    s.reopen_item(None, &a.reference, Some("revisiting"), None)
        .unwrap();
    let reopened = s.get_item(None, &a.reference, &[Include::Lineage]).unwrap();
    assert_eq!(reopened.status, Status::Discovery);
    assert!(reopened.archived_at.is_none());
    let kinds: Vec<EventType> = reopened
        .lineage
        .unwrap()
        .iter()
        .map(|e| e.event_type)
        .collect();
    assert!(kinds.contains(&EventType::Reopen));
}

#[test]
fn search_matches_title_and_body() {
    let s = store();
    s.add_item(
        None,
        NewItem {
            title: "Token refresh".into(),
            body: Some("handle 401 then retry".into()),
            ..Default::default()
        },
    )
    .unwrap();
    add(&s, "Dark mode");
    let hits = s.search(None, "refresh", None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].reference, "HV-1");
    // FTS stays consistent after an update removes the matching term.
    s.update_item(
        None,
        "HV-1",
        ItemUpdate {
            title: Some("Billing".into()),
            body: Some("invoices".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.search(None, "refresh", None).unwrap().len(), 0);
}

#[test]
fn rank_orders_relative_to_siblings() {
    let s = store();
    let a = s
        .add_item(
            None,
            NewItem {
                title: "A".into(),
                commit: true,
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    let b = s
        .add_item(
            None,
            NewItem {
                title: "B".into(),
                commit: true,
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    let c = s
        .add_item(
            None,
            NewItem {
                title: "C".into(),
                commit: true,
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

    // Establish an order: rank B after A, C after B.
    s.rank_item(None, &b.reference, None, Some(&a.reference))
        .unwrap();
    s.rank_item(None, &c.reference, None, Some(&b.reference))
        .unwrap();
    let order = s
        .list_items(
            None,
            &ItemFilter {
                committed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let refs: Vec<&str> = order.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(
        refs,
        vec![
            a.reference.as_str(),
            b.reference.as_str(),
            c.reference.as_str()
        ]
    );

    // Move C before A.
    s.rank_item(None, &c.reference, Some(&a.reference), None)
        .unwrap();
    let order = s
        .list_items(
            None,
            &ItemFilter {
                committed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let refs: Vec<&str> = order.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(
        refs,
        vec![
            c.reference.as_str(),
            a.reference.as_str(),
            b.reference.as_str()
        ]
    );
}

#[test]
fn rank_is_scoped_to_the_targets_band() {
    let s = store();
    // A band-0 item whose key sits "between" the band-1 items must not interfere.
    let hi = s
        .add_item(
            None,
            NewItem {
                title: "urgent".into(),
                commit: true,
                priority: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    let a = s
        .add_item(
            None,
            NewItem {
                title: "A".into(),
                commit: true,
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    let b = s
        .add_item(
            None,
            NewItem {
                title: "B".into(),
                commit: true,
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    s.rank_item(None, &b.reference, None, Some(&a.reference))
        .unwrap();
    // Rank the band-0 item relative to itself-band peers is irrelevant; assert the
    // band-1 order is purely a/b and the band-0 item leads by priority.
    let order = s
        .list_items(
            None,
            &ItemFilter {
                committed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let refs: Vec<&str> = order.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(
        refs,
        vec![
            hi.reference.as_str(),
            a.reference.as_str(),
            b.reference.as_str()
        ]
    );

    // NULL-priority band: ranking among unprioritised items uses NULL-safe IS.
    let x = add(&s, "X");
    let y = add(&s, "Y");
    s.rank_item(None, &y.reference, Some(&x.reference), None)
        .unwrap(); // y before x
    let ice = s
        .list_items(
            None,
            &ItemFilter {
                icebox: true,
                ..Default::default()
            },
        )
        .unwrap();
    let null_refs: Vec<&str> = ice
        .iter()
        .filter(|i| i.priority.is_none())
        .map(|i| i.reference.as_str())
        .collect();
    let yi = null_refs.iter().position(|r| *r == y.reference).unwrap();
    let xi = null_refs.iter().position(|r| *r == x.reference).unwrap();
    assert!(
        yi < xi,
        "y should sort before x within the unprioritised band"
    );
}

#[test]
fn icebox_filter_excludes_committed_and_dead() {
    let s = store();
    let _floating = add(&s, "idea");
    let committed = s
        .add_item(
            None,
            NewItem {
                title: "doing".into(),
                commit: true,
                ..Default::default()
            },
        )
        .unwrap();
    let archived = add(&s, "dead");
    s.archive_item(None, &archived.reference, None, None)
        .unwrap();

    let ice = s
        .list_items(
            None,
            &ItemFilter {
                icebox: true,
                ..Default::default()
            },
        )
        .unwrap();
    let refs: Vec<&str> = ice.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(refs, vec!["HV-1"]);
    assert!(!refs.contains(&committed.reference.as_str()));
}

#[test]
fn graph_walks_lineage_in_both_directions() {
    let s = store();
    let big = add(&s, "Big");
    let parts = s
        .evolve_split(
            None,
            &big.reference,
            &["P1".into(), "P2".into()],
            None,
            None,
        )
        .unwrap();
    let p1 = &parts.new[0].reference;

    let anc = s
        .evolve_graph(None, p1, LineageDirection::Ancestors, None)
        .unwrap();
    assert_eq!(anc.events.len(), 1);
    assert_eq!(anc.events[0].event_type, EventType::Split);

    let desc = s
        .evolve_graph(None, &big.reference, LineageDirection::Descendants, None)
        .unwrap();
    assert_eq!(desc.events.len(), 1);
}

#[test]
fn status_counts() {
    let s = store();
    add(&s, "one");
    s.add_item(
        None,
        NewItem {
            title: "two".into(),
            commit: true,
            ..Default::default()
        },
    )
    .unwrap();
    let st = s.store_status(None).unwrap();
    assert_eq!(st["total"], 2);
    assert_eq!(st["committed"], 1);
    assert_eq!(st["icebox"], 1);
    // Everything is freshly created and unsynced.
    assert!(st["sync_pending"].as_i64().unwrap() >= 2);
}
