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
fn handoff_records_artifact_flips_owner_and_sets_state() {
    // A real content root: handoff writes a `handoff` artifact to disk.
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_in_memory_at(dir.path()).unwrap();
    s.add_project("haven", Some("HV"), "Haven", None).unwrap();
    s.use_project("haven").unwrap();
    let item = s
        .add_item(
            None,
            NewItem {
                title: "Build API".into(),
                assign: Some(OwnerKind::Ai),
                status: Some(Status::Ready),
                done_looks_like: Some("API returns 200".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(item.reference, "HV-1");

    // ai → human with a note: parks it blocked + on_human, records the baton.
    let res = s
        .handoff(
            None,
            "HV-1",
            OwnerKind::Human,
            HandoffInput {
                note: Some("Implemented; please review rate-limit defaults."),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(res.item.owner_kind, Some(OwnerKind::Human));
    assert_eq!(res.item.status, Status::Blocked);
    assert_eq!(res.item.wait_state, Some(WaitState::OnHuman));
    let art = res.artifact.expect("a note records a handoff artifact");
    assert_eq!(art.role, ArtifactRole::Handoff);
    assert_eq!(art.from_owner, Some(OwnerKind::Ai));
    assert_eq!(art.to_owner, Some(OwnerKind::Human));
    assert!(art
        .path
        .as_deref()
        .unwrap()
        .starts_with("items/HV-1/notes/handoff-"));

    // human → ai, no note: owner flips, wait clears, blocked becomes ready again.
    let back = s
        .handoff(None, "HV-1", OwnerKind::Ai, HandoffInput::default())
        .unwrap();
    assert_eq!(back.item.owner_kind, Some(OwnerKind::Ai));
    assert_eq!(back.item.wait_state, None);
    assert_eq!(back.item.status, Status::Ready);
    assert!(back.artifact.is_none());
}

#[test]
fn complete_records_evidence_marks_done_and_reports_unblocked() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_in_memory_at(dir.path()).unwrap();
    s.add_project("haven", Some("HV"), "Haven", None).unwrap();
    s.use_project("haven").unwrap();

    // A (HV-1) is a dependency of B (HV-2) and C (HV-4); C also depends on the
    // still-open D (HV-3). Completing A should unblock B but not C.
    s.add_item(
        None,
        NewItem {
            title: "A".into(),
            done_looks_like: Some("tests pass".into()),
            ..Default::default()
        },
    )
    .unwrap();
    s.add_item(
        None,
        NewItem {
            title: "B".into(),
            depends_on: Some("HV-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    add(&s, "D"); // HV-3
    s.add_item(
        None,
        NewItem {
            title: "C".into(),
            depends_on: Some("HV-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    s.depend(None, "HV-4", "HV-3", false).unwrap(); // C also blocked by D

    let res = s
        .complete_item(
            None,
            "HV-1",
            CompleteInput {
                evidence: Some("cargo test --workspace: ok"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(res.item.status, Status::Done);
    assert!(
        res.warnings.is_empty(),
        "acceptance was set: {:?}",
        res.warnings
    );
    assert_eq!(res.artifact.unwrap().role, ArtifactRole::Delivery);
    let unblocked: Vec<&str> = res.unblocked.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(unblocked, ["HV-2"], "B unblocks; C still waits on D");

    // No-acceptance path warns but still completes; no evidence → no artifact.
    let r2 = s
        .complete_item(None, "HV-3", CompleteInput::default())
        .unwrap();
    assert!(!r2.warnings.is_empty());
    assert!(r2.artifact.is_none());

    // Completing an archived item is refused (reopen first).
    add(&s, "E"); // HV-5
    s.archive_item(None, "HV-5", None, None).unwrap();
    assert!(s
        .complete_item(None, "HV-5", CompleteInput::default())
        .is_err());
}

#[test]
fn anchors_are_living_docs_not_dispatch_work() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_in_memory_at(dir.path()).unwrap();
    s.add_project("haven", Some("HV"), "Haven", None).unwrap();
    s.use_project("haven").unwrap();

    let anchor = s
        .add_item(
            None,
            NewItem {
                title: "Haven docs".into(),
                node_type: Some(NodeType::Anchor),
                status: Some(Status::Ready),
                done_looks_like: Some("docs landed".into()),
                commit: true,
                assign: Some(OwnerKind::Ai),
                ..Default::default()
            },
        )
        .unwrap();
    let work = s
        .add_item(
            None,
            NewItem {
                title: "Build feature".into(),
                status: Some(Status::Ready),
                done_looks_like: Some("feature works".into()),
                commit: true,
                assign: Some(OwnerKind::Ai),
                ..Default::default()
            },
        )
        .unwrap();

    let next = s.next(None, Some(OwnerKind::Ai), None).unwrap();
    let refs: Vec<&str> = next.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(refs, [work.reference.as_str()]);
    let explain = s.next_explain(None, Some(OwnerKind::Ai)).unwrap();
    assert_eq!(explain["dispatchable"], 1);

    s.conn
        .execute(
            "UPDATE nodes SET updated_at = datetime('now','-30 days') WHERE ref = ?1",
            rusqlite::params![anchor.reference],
        )
        .unwrap();
    let stale = s
        .list_items(
            None,
            &ItemFilter {
                stale_days: Some(7),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!stale.iter().any(|i| i.reference == anchor.reference));

    assert!(s
        .rank_item(None, &anchor.reference, None, Some(&work.reference))
        .is_err());
    assert!(s
        .rank_item(None, &work.reference, None, Some(&anchor.reference))
        .is_err());

    let artifact = s
        .add_artifact(
            None,
            &anchor.reference,
            NewArtifact {
                role: ArtifactRole::Vision,
                kind: ArtifactKind::File,
                content: Some("Project vision".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(artifact.role, ArtifactRole::Vision);
    let docs = s.docs(None).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].item.reference, anchor.reference);
    assert_eq!(docs[0].artifacts.len(), 1);

    let complete = s
        .complete_item(None, &anchor.reference, CompleteInput::default())
        .unwrap_err();
    assert_eq!(complete.code(), "invalid");
    assert!(complete.to_string().contains("artifact-bearing anchor"));
    let archive = s
        .archive_item(None, &anchor.reference, Some("cleanup"), None)
        .unwrap_err();
    assert_eq!(archive.code(), "invalid");
    assert!(archive.to_string().contains("artifact-bearing anchor"));

    let empty_anchor = s
        .add_item(
            None,
            NewItem {
                title: "Empty docs anchor".into(),
                node_type: Some(NodeType::Anchor),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        s.archive_item(None, &empty_anchor.reference, Some("unused"), None)
            .unwrap()
            .status,
        Status::Archived
    );
}

#[test]
fn batch_commit_uncommit_archive_validate_refs_first() {
    let s = store();
    add(&s, "A"); // HV-1
    add(&s, "B"); // HV-2
    add(&s, "C"); // HV-3

    // "commit these two" → one op, both committed at the band.
    let committed = s.commit_items(None, &["HV-1", "HV-2"], Some(2)).unwrap();
    assert_eq!(committed.len(), 2);
    assert!(committed
        .iter()
        .all(|i| i.committed && i.priority == Some(2)));

    // "archive those" → batch archive.
    let archived = s
        .archive_items(None, &["HV-1", "HV-3"], Some("groomed away"), None)
        .unwrap();
    assert!(archived.iter().all(|i| i.status == Status::Archived));

    // Validate-first: an unknown ref aborts the WHOLE batch — HV-2 is untouched,
    // not half-archived.
    assert!(s
        .archive_items(None, &["HV-2", "HV-404"], None, None)
        .is_err());
    assert_eq!(
        s.get_item(None, "HV-2", &[]).unwrap().status,
        Status::Discovery
    );

    // Batch uncommit.
    let unc = s.uncommit_items(None, &["HV-2"]).unwrap();
    assert!(!unc[0].committed);
}

#[test]
fn batch_update_applies_one_change_to_many_refs() {
    let s = store();
    add(&s, "A"); // HV-1
    add(&s, "B"); // HV-2
    add(&s, "C"); // HV-3

    // "mark these two ready" with an acceptance, in one op.
    let res = s
        .update_items(
            None,
            &["HV-1", "HV-2"],
            ItemUpdate {
                status: Some(Status::Ready),
                done_looks_like: Some("ship it".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(res
        .iter()
        .all(|i| i.status == Status::Ready && i.done_looks_like.as_deref() == Some("ship it")));
    // HV-3 untouched.
    assert_eq!(
        s.get_item(None, "HV-3", &[]).unwrap().status,
        Status::Discovery
    );

    // Validate-first: an unknown ref aborts the batch; HV-3 stays as it was.
    assert!(s
        .update_items(
            None,
            &["HV-3", "HV-404"],
            ItemUpdate {
                status: Some(Status::Ready),
                ..Default::default()
            },
        )
        .is_err());
    assert_eq!(
        s.get_item(None, "HV-3", &[]).unwrap().status,
        Status::Discovery
    );
}

#[test]
fn list_filters_by_wait_state_and_staleness() {
    let s = store();
    let waiting = add(&s, "Review PR"); // HV-1
    s.update_item(
        None,
        &waiting.reference,
        ItemUpdate {
            wait: Some(WaitUpdate::Set(WaitState::OnHuman)),
            ..Default::default()
        },
    )
    .unwrap();
    let fresh = add(&s, "Fresh task"); // HV-2

    // "what's waiting on me?" → only the on_human item.
    let waits = s
        .list_items(
            None,
            &ItemFilter {
                wait: Some(WaitState::OnHuman),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        waits
            .iter()
            .map(|i| i.reference.as_str())
            .collect::<Vec<_>>(),
        [waiting.reference.as_str()]
    );

    // Backdate the fresh item 10 days; --stale 7 surfaces it, --stale 14 doesn't.
    s.conn
        .execute(
            "UPDATE nodes SET updated_at = datetime('now','-10 days') WHERE ref = ?1",
            rusqlite::params![fresh.reference],
        )
        .unwrap();
    let stale = s
        .list_items(
            None,
            &ItemFilter {
                stale_days: Some(7),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        stale
            .iter()
            .map(|i| i.reference.as_str())
            .collect::<Vec<_>>(),
        [fresh.reference.as_str()]
    );
    assert!(s
        .list_items(
            None,
            &ItemFilter {
                stale_days: Some(14),
                ..Default::default()
            },
        )
        .unwrap()
        .is_empty());
}

#[test]
fn project_graph_returns_all_nodes_and_edges_in_one_call() {
    let s = store();
    add(&s, "A"); // HV-1
    add(&s, "B"); // HV-2
    add(&s, "C"); // HV-3
    s.add_item(
        None,
        NewItem {
            title: "v1".into(),
            node_type: Some(NodeType::Release),
            ..Default::default()
        },
    )
    .unwrap(); // HV-4
    add(&s, "D"); // HV-5
    s.decompose(None, "HV-1", "HV-2", false).unwrap(); // A composed of B
    s.depend(None, "HV-2", "HV-3", false).unwrap(); // B depends on C
    s.group(None, "HV-4", "HV-1", false).unwrap(); // release groups A
    s.evolve_supersede(None, "HV-3", "HV-5", Some("redesign"), None)
        .unwrap(); // HV-3 → HV-5

    let g = s.project_graph(None, true).unwrap();
    assert_eq!(g.project, "haven");
    // Every node is present, including the superseded HV-3 (faithful dump).
    assert_eq!(g.nodes.len(), 5);
    // The three structural edges, each as {kind, from, to} matching add_edge.
    assert_eq!(g.edges.len(), 3);
    assert!(g
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Decomposition && e.from == "HV-1" && e.to == "HV-2"));
    assert!(g
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Dependency && e.from == "HV-2" && e.to == "HV-3"));
    assert!(g
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Grouping && e.from == "HV-4" && e.to == "HV-1"));
    // Lineage link from the supersession is included when requested.
    assert!(g.lineage.iter().any(|l| l.from == "HV-3" && l.to == "HV-5"));

    // Without the flag, lineage is omitted (lean payload).
    assert!(s.project_graph(None, false).unwrap().lineage.is_empty());
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
fn ready_requires_acceptance() {
    // HV-80: `ready` requires `done_looks_like` — enforced at the store, not
    // just asserted by the skill. Covers both creation and the transition.
    let s = store();

    // add_item: an item cannot be born `ready` without acceptance.
    let err = s
        .add_item(
            None,
            NewItem {
                title: "naked ready".into(),
                status: Some(Status::Ready),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("done_looks_like"));

    // update_item: status→ready without acceptance is refused...
    let item = add(&s, "groom me");
    let err = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                status: Some(Status::Ready),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(err.to_string().contains("done_looks_like"));

    // ...and succeeds once acceptance is supplied in the same op.
    let ok = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                status: Some(Status::Ready),
                done_looks_like: Some("it works".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(ok.status, Status::Ready);

    // Clearing acceptance (whitespace) on an already-ready item is refused.
    let err = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                done_looks_like: Some("   ".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid");

    // Unrelated edits to a ready item are unaffected by the guard.
    let ok = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                priority: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(ok.priority, Some(1));
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
                done_looks_like: Some("child done".into()),
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
                done_looks_like: Some("parked done".into()),
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
                done_looks_like: Some("waiting done".into()),
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
                done_looks_like: Some("blocked done".into()),
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
                done_looks_like: Some("go done".into()),
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
                done_looks_like: Some("ai work done".into()),
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
                done_looks_like: Some("human work done".into()),
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
fn inbox_excludes_committed_dead_and_sealed_then_drops_on_triage() {
    let s = store();
    // An untriaged floater: uncommitted, no acceptance — the one inbox item.
    let floater = add(&s, "idea");
    // A sealed floater: uncommitted but already scoped — excluded from inbox.
    let sealed = s
        .add_item(
            None,
            NewItem {
                title: "scoped".into(),
                done_looks_like: Some("it ships".into()),
                ..Default::default()
            },
        )
        .unwrap();
    // Committed and archived are excluded (same as icebox).
    let _committed = s
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

    let inbox = |s: &Store| {
        s.list_items(
            None,
            &ItemFilter {
                inbox: true,
                ..Default::default()
            },
        )
        .unwrap()
    };

    let refs: Vec<String> = inbox(&s).iter().map(|i| i.reference.clone()).collect();
    assert_eq!(refs, vec![floater.reference.clone()]);
    assert!(!refs.contains(&sealed.reference));

    // Triaging the floater (giving it acceptance) drops it out of the inbox.
    s.update_item(
        None,
        &floater.reference,
        ItemUpdate {
            done_looks_like: Some("a real definition".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(inbox(&s).is_empty());
}

#[test]
fn container_rollup_derives_from_committed_subtree() {
    let s = store();
    let phase = s
        .add_item(
            None,
            NewItem {
                title: "Phase".into(),
                node_type: Some(NodeType::Phase),
                ..Default::default()
            },
        )
        .unwrap();
    let rollup = |s: &Store| {
        s.get_item(None, &phase.reference, &[])
            .unwrap()
            .rollup_state
    };

    // Two uncommitted children via decomposition → nothing committed yet.
    let a = s
        .add_item(
            None,
            NewItem {
                title: "A".into(),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let b = s
        .add_item(
            None,
            NewItem {
                title: "B".into(),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(rollup(&s), Some(RollupState::Dormant));

    // Commit both → queued (committed work, none started).
    s.commit_items(None, &[a.reference.as_str(), b.reference.as_str()], None)
        .unwrap();
    assert_eq!(rollup(&s), Some(RollupState::Queued));

    // One in_progress → active.
    s.update_item(
        None,
        &a.reference,
        ItemUpdate {
            status: Some(Status::InProgress),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rollup(&s), Some(RollupState::Active));

    // All committed descendants done → done.
    for r in [&a.reference, &b.reference] {
        s.update_item(
            None,
            r,
            ItemUpdate {
                status: Some(Status::Done),
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(rollup(&s), Some(RollupState::Done));

    // A committed, in_progress node reached via BOTH a grouping and a
    // decomposition edge is counted once and pulls the rollup back to active —
    // exercising the union walk + dedup.
    let _c = s
        .add_item(
            None,
            NewItem {
                title: "C".into(),
                commit: true,
                status: Some(Status::InProgress),
                parent: Some(phase.reference.clone()),
                group: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(rollup(&s), Some(RollupState::Active));

    // project_graph agrees with get_item for the same container.
    let g = s.project_graph(None, false).unwrap();
    let from_graph = g
        .nodes
        .iter()
        .find(|n| n.reference == phase.reference)
        .unwrap()
        .rollup_state;
    assert_eq!(from_graph, Some(RollupState::Active));

    // A leaf carries no rollup.
    assert_eq!(
        s.get_item(None, &a.reference, &[]).unwrap().rollup_state,
        None
    );
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

// ---- HV-17: idempotent capture + bulk import -------------------------------

#[test]
fn normalize_title_folds_case_whitespace_and_trailing_punctuation() {
    use super::item::normalize_title as norm;
    assert_eq!(norm("Setup  CI."), "setup ci");
    assert_eq!(norm("  Setup\tCI  "), "setup ci");
    assert_eq!(norm("Ship it!?"), "ship it");
    assert_eq!(norm("Ship it !?"), "ship it");
    assert_eq!(norm("A.B"), "a.b"); // only trailing punctuation strips
    assert_eq!(norm("???"), ""); // empty-normalized: never matches
}

#[test]
fn fts_title_query_neutralizes_fts_syntax() {
    use super::item::fts_title_query as q;
    assert_eq!(
        q(r#"Fix "auth" — (don't crash)!"#).unwrap(),
        r#"title : ("Fix" OR "auth" OR "don" OR "t" OR "crash")"#
    );
    assert_eq!(
        q("AND OR NOT").unwrap(),
        r#"title : ("AND" OR "OR" OR "NOT")"#
    );
    assert_eq!(q("?!— ()"), None);
}

#[test]
fn add_if_absent_returns_existing_and_ignores_dead_matches() {
    let s = store();
    let first = add(&s, "Setup CI");

    // Sloppier casing/whitespace/punctuation still hits.
    let out = s
        .add_item_checked(
            None,
            NewItem {
                title: "  setup  ci.".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap();
    assert!(out.existing);
    assert_eq!(out.item.reference, first.reference);
    assert_eq!(s.store_status(None).unwrap()["total"], 1);

    // An archived match is dead — a new item is created.
    s.archive_item(None, "HV-1", Some("rot"), None).unwrap();
    let out = s
        .add_item_checked(
            None,
            NewItem {
                title: "Setup CI".into(),
                ..Default::default()
            },
            true,
        )
        .unwrap();
    assert!(!out.existing);
    assert_ne!(out.item.reference, first.reference);
}

#[test]
fn add_reports_similar_titles_capped_and_punctuation_safe() {
    let s = store();
    add(&s, "User login flow");
    let out = s
        .add_item_checked(
            None,
            NewItem {
                title: "Login flow for users".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert!(!out.existing);
    assert!(out.similar.iter().any(|x| x.reference == "HV-1"));
    // Never lists itself.
    assert!(out
        .similar
        .iter()
        .all(|x| x.reference != out.item.reference));

    for i in 0..5 {
        add(&s, &format!("Login flow variant {i}"));
    }
    let out = s
        .add_item_checked(
            None,
            NewItem {
                title: "Another login flow".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(out.similar.len(), 3); // capped

    // A punctuation bomb neither errors nor matches anything.
    let out = s
        .add_item_checked(
            None,
            NewItem {
                title: "?!— ()".into(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert!(out.similar.is_empty());
}

fn import_batch(v: serde_json::Value) -> Vec<ImportItem> {
    serde_json::from_value(v).unwrap()
}

#[test]
fn import_wires_temp_id_edges_in_one_batch() {
    let s = store();
    add(&s, "Pre-existing"); // HV-1

    let outcomes = s
        .import_items(
            None,
            import_batch(serde_json::json!([
                // Forward reference: parent "epic" appears later in the file.
                {"id": "api", "title": "Build API", "parent": "epic",
                 "depends_on": ["HV-1"], "status": "ready", "commit": true},
                {"id": "ui", "title": "Build UI", "depends_on": ["api"], "group": "phase1"},
                {"id": "epic", "title": "Auth epic"},
                {"id": "phase1", "title": "Phase 1", "type": "phase"}
            ])),
            false,
        )
        .unwrap();

    assert_eq!(outcomes.len(), 4);
    assert_eq!(outcomes[0].id.as_deref(), Some("api"));
    assert_eq!(outcomes[0].item.reference, "HV-2");
    assert_eq!(outcomes[3].item.reference, "HV-5"); // sequential refs
    assert!(outcomes.iter().all(|o| !o.existing));

    let g = s.project_graph(None, false).unwrap();
    let has = |kind: EdgeKind, from: &str, to: &str| {
        g.edges
            .iter()
            .any(|e| e.kind == kind && e.from == from && e.to == to)
    };
    assert!(has(EdgeKind::Decomposition, "HV-4", "HV-2")); // epic ⊃ api
    assert!(has(EdgeKind::Dependency, "HV-2", "HV-1")); // api → pre-existing
    assert!(has(EdgeKind::Dependency, "HV-3", "HV-2")); // ui → api (temp id)
    assert!(has(EdgeKind::Grouping, "HV-5", "HV-3")); // phase1 ∋ ui
}

#[test]
fn import_is_all_or_nothing_on_edge_failure() {
    let s = store();
    add(&s, "Anchor"); // HV-1; ref_counter = 1

    // In-batch dependency cycle: only detectable at edge-wiring time.
    let err = s
        .import_items(
            None,
            import_batch(serde_json::json!([
                {"id": "a", "title": "First", "depends_on": ["b"]},
                {"id": "b", "title": "Second", "depends_on": ["a"]}
            ])),
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("cycle"), "got: {err}");

    // Nothing persisted: node count AND the minted ref counter rolled back.
    assert_eq!(s.store_status(None).unwrap()["total"], 1);
    assert_eq!(s.get_project("haven").unwrap().ref_counter, 1);
}

#[test]
fn import_validation_rejects_bad_input_without_writing() {
    let s = store();
    add(&s, "Anchor"); // HV-1

    let cases = [
        // Duplicate temp ids.
        serde_json::json!([{"id": "x", "title": "One"}, {"id": "x", "title": "Two"}]),
        // Temp id shadowing a real ref.
        serde_json::json!([{"id": "HV-1", "title": "Shadow"}]),
        // Bad enum.
        serde_json::json!([{"title": "Bad", "status": "wat"}]),
        // Unknown edge target.
        serde_json::json!([{"title": "Dangling", "depends_on": ["nope"]}]),
        // Empty title.
        serde_json::json!([{"title": "   "}]),
    ];
    for case in cases {
        let err = s.import_items(None, import_batch(case.clone()), false);
        assert!(err.is_err(), "case should fail: {case}");
    }
    assert_eq!(s.store_status(None).unwrap()["total"], 1);
    assert_eq!(s.get_project("haven").unwrap().ref_counter, 1);
}

#[test]
fn import_if_absent_dedupes_against_store_and_batch() {
    let s = store();
    add(&s, "Setup CI"); // HV-1

    let outcomes = s
        .import_items(
            None,
            import_batch(serde_json::json!([
                {"id": "ci", "title": "setup  ci."},
                {"title": "Run tests", "depends_on": ["ci"]},
                {"title": "SETUP CI"}
            ])),
            true,
        )
        .unwrap();

    // Batch item 0 matched the pre-existing node; its temp id resolved there.
    assert!(outcomes[0].existing);
    assert_eq!(outcomes[0].item.reference, "HV-1");
    // Batch item 2 matched within the batch (same normalized title).
    assert!(outcomes[2].existing);
    assert_eq!(outcomes[2].item.reference, "HV-1");
    // Only "Run tests" was created, depending on the matched pre-existing node.
    assert!(!outcomes[1].existing);
    assert_eq!(s.store_status(None).unwrap()["total"], 2);
    let g = s.project_graph(None, false).unwrap();
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Dependency
        && e.from == outcomes[1].item.reference
        && e.to == "HV-1"));
}
