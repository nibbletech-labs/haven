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

/// A container node (release/phase/gate) — a valid grouping target.
fn container(s: &Store, title: &str, node_type: NodeType) -> Item {
    s.add_item(
        None,
        NewItem {
            title: title.into(),
            node_type: Some(node_type),
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
    // Both are assigned to ai (owner_kind=ai), the axis `next --owner ai` filters
    // (HV-125). The anchor must STILL be excluded — by TYPE, not ownership: proves
    // the exclusion is type-driven, not luck.
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
    // HV-126: superseding HV-3 with HV-5 forwarded HV-2's dependency onto HV-5 (the
    // live replacement) and dropped the dangling edge to the superseded HV-3.
    assert!(g
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Dependency && e.from == "HV-2" && e.to == "HV-5"));
    assert!(
        !g.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Dependency && e.to == "HV-3"),
        "the dependency on the superseded HV-3 must not survive"
    );
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
fn grooming_pressure_nudges_above_threshold() {
    // HV-82: grooming is triggered, not just surfaced — once untriaged work
    // piles up, the nudge appears (and rides the graph a planner reorients on).
    let s = store();

    // Below threshold: counts climb but no nudge yet.
    for i in 0..(GROOMING_NUDGE_THRESHOLD - 1) {
        add(&s, &format!("floater {i}"));
    }
    let p = s.grooming_pressure(None).unwrap();
    assert_eq!(p.untriaged, GROOMING_NUDGE_THRESHOLD - 1);
    assert!(p.nudge.is_none());
    assert!(s
        .project_graph(None, false)
        .unwrap()
        .grooming_nudge
        .is_none());

    // Crossing the threshold emits the nudge, surfaced on the graph too.
    add(&s, "one more floater");
    let p = s.grooming_pressure(None).unwrap();
    assert_eq!(p.untriaged, GROOMING_NUDGE_THRESHOLD);
    assert!(p.nudge.is_some());
    assert!(s
        .project_graph(None, false)
        .unwrap()
        .grooming_nudge
        .is_some());
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

/// Create a ready+committed leaf assigned to `owner` — the planner-seal shape
/// (ready + committed + acceptance + owner_kind), the axis `next --owner` filters
/// (HV-125). `owner = None` leaves it unassigned (NULL owner_kind). Returns the ref.
fn ready_assigned(s: &Store, title: &str, owner: Option<OwnerKind>) -> String {
    s.add_item(
        None,
        NewItem {
            title: title.into(),
            status: Some(Status::Ready),
            done_looks_like: Some(format!("{title} done")),
            commit: true,
            assign: owner,
            ..Default::default()
        },
    )
    .unwrap()
    .reference
}

/// HV-125 headline: in ONE project with ready+committed leaves, `next --owner ai`
/// surfaces only the ai-assigned leaf; `--owner human` only the human-assigned
/// one; an unassigned (NULL owner_kind) leaf is in NEITHER owner query; and bare
/// `next` (no owner) ignores ownership entirely.
#[test]
fn next_owner_kind_dispatch_matrix() {
    let s = store();
    let ai = ready_assigned(&s, "ai work", Some(OwnerKind::Ai));
    let human = ready_assigned(&s, "human work", Some(OwnerKind::Human));
    let unassigned = ready_assigned(&s, "unassigned work", None);

    let refs = |items: &[Item]| -> std::collections::HashSet<String> {
        items.iter().map(|i| i.reference.clone()).collect()
    };

    // --owner ai → {ai}; never human, never unassigned.
    let ai_q = refs(&s.next(None, Some(OwnerKind::Ai), None).unwrap());
    assert_eq!(
        ai_q,
        std::collections::HashSet::from([ai.clone()]),
        "next --owner ai must be exactly the ai-assigned leaf"
    );
    assert!(!ai_q.contains(&human));
    assert!(!ai_q.contains(&unassigned));

    // --owner human → {human}; never ai, never unassigned.
    let human_q = refs(&s.next(None, Some(OwnerKind::Human), None).unwrap());
    assert_eq!(
        human_q,
        std::collections::HashSet::from([human.clone()]),
        "next --owner human must be exactly the human-assigned leaf"
    );
    assert!(!human_q.contains(&ai));
    assert!(!human_q.contains(&unassigned));

    // Bare next (no --owner) ignores ownership — all three surface.
    let bare = refs(&s.next(None, None, None).unwrap());
    assert_eq!(
        bare,
        std::collections::HashSet::from([ai, human, unassigned]),
        "bare next must ignore ownership and return all three ready leaves"
    );
}

/// HV-125 lockstep: `next --explain`'s dispatchable count must equal the real
/// `next` length for each owner — the `owner_kind` predicate is applied at the
/// one shared seam in BOTH `next()` and `count_dispatchable`.
#[test]
fn next_explain_dispatchable_matches_next_per_owner() {
    let s = store();
    ready_assigned(&s, "ai work", Some(OwnerKind::Ai));
    ready_assigned(&s, "human work", Some(OwnerKind::Human));
    ready_assigned(&s, "unassigned work", None);

    for owner in [None, Some(OwnerKind::Ai), Some(OwnerKind::Human)] {
        let n = s.next(None, owner, None).unwrap().len() as i64;
        let explain = s.next_explain(None, owner).unwrap();
        assert_eq!(
            explain["dispatchable"].as_i64().unwrap(),
            n,
            "next_explain.dispatchable must equal next().len() for owner {owner:?}"
        );
    }
}

/// HV-125 three-valued logic: an unassigned (NULL owner_kind) ready leaf is absent
/// from `next --owner ai`/`human` directly (`owner_kind = ?` against NULL yields
/// NULL ⇒ excluded), even though it surfaces in bare `next`.
#[test]
fn next_owner_excludes_unassigned_null_owner_kind() {
    let s = store();
    let unassigned = ready_assigned(&s, "unassigned", None);

    let ai_q = s.next(None, Some(OwnerKind::Ai), None).unwrap();
    assert!(
        !ai_q.iter().any(|i| i.reference == unassigned),
        "an unassigned leaf must never appear in a --owner ai query"
    );
    let human_q = s.next(None, Some(OwnerKind::Human), None).unwrap();
    assert!(
        !human_q.iter().any(|i| i.reference == unassigned),
        "an unassigned leaf must never appear in a --owner human query"
    );
    // But it IS dispatchable to a bare (owner-agnostic) next.
    let bare = s.next(None, None, None).unwrap();
    assert!(bare.iter().any(|i| i.reference == unassigned));
}

/// HV-125 dispatch invariant (the plan->run contract the suite previously lacked):
/// a planner-sealed leaf — ready + committed + done_looks_like + owner_kind=ai —
/// MUST be dispatchable via `next --owner ai`. orchestrate-plan's SKILL.md
/// guarantees "next --owner ai is the AI dispatch queue"; this is the guard that
/// the producer (assignment) and the consumer (`next --owner`) filter the SAME
/// axis — the exact regression owner_eligible (HV-66) silently introduced.
#[test]
fn next_owner_ai_dispatches_planner_sealed_leaf() {
    let s = store();
    let sealed = ready_assigned(&s, "sealed ai leaf", Some(OwnerKind::Ai));

    let ai_q = s.next(None, Some(OwnerKind::Ai), None).unwrap();
    assert!(
        ai_q.iter().any(|i| i.reference == sealed),
        "a planner-sealed owner_kind=ai leaf must be dispatchable via next --owner ai"
    );
    let explain = s.next_explain(None, Some(OwnerKind::Ai)).unwrap();
    assert_eq!(
        explain["dispatchable"], 1,
        "next --explain must agree the sealed leaf is dispatchable"
    );
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

/// HV-126: evolve merge forwards the sources' structural edges onto the survivor.
/// The survivor inherits the decomposition parent + the inbound dependency (so it
/// is not orphaned), and a dependent of a merged-away prerequisite stays blocked —
/// dispatch must NOT treat the superseded source as satisfied — until the survivor
/// is completed.
#[test]
fn merge_forwards_edges_and_keeps_dependents_blocked() {
    let s = store();
    let parent = add(&s, "Parent epic");
    let s1 = ready_assigned(&s, "source one", None);
    let s2 = ready_assigned(&s, "source two", None);
    let dep = ready_assigned(&s, "dependent", None);
    s.decompose(None, &parent.reference, &s1, false).unwrap();
    s.decompose(None, &parent.reference, &s2, false).unwrap();
    s.depend(None, &dep, &s1, false).unwrap(); // dep depends on s1

    let res = s
        .evolve_merge(None, &[s1.clone(), s2.clone()], "Unified", None, None)
        .unwrap();
    let survivor = res.new[0].reference.clone();

    // Survivor inherits BOTH the parent (decomposition) and the dependent (blocks) —
    // it is not the orphaned zero-edge node HV-126 describes.
    let edges = s
        .get_item(None, &survivor, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        edges.parents.contains(&parent.reference),
        "survivor must inherit the decomposition parent (not orphaned)"
    );
    assert!(
        edges.blocks.contains(&dep),
        "survivor must inherit the dependent (dep now depends on survivor)"
    );

    // The merged-away source is stripped of its structural edges.
    let s1_edges = s
        .get_item(None, &s1, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        s1_edges.parents.is_empty() && s1_edges.blocks.is_empty(),
        "a merged-away source keeps no structural edges"
    );

    // dep now depends on the live survivor, not the superseded source.
    let dep_edges = s
        .get_item(None, &dep, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert_eq!(
        dep_edges.depends_on,
        vec![survivor.clone()],
        "dependent re-pointed onto the survivor"
    );

    // Dispatch: dep must NOT be dispatchable while the survivor is incomplete.
    // (Before HV-126, merging s1 away silently made dep dispatchable — the dispatch
    // predicate treats a superseded prerequisite as satisfied.)
    let blocked: Vec<String> = s
        .next(None, None, None)
        .unwrap()
        .into_iter()
        .map(|i| i.reference)
        .collect();
    assert!(
        !blocked.contains(&dep),
        "dependent must stay blocked on the live survivor"
    );

    // Ready + commit + complete the survivor → dep unblocks.
    s.update_item(
        None,
        &survivor,
        ItemUpdate {
            status: Some(Status::Ready),
            done_looks_like: Some("merged work done".into()),
            ..Default::default()
        },
    )
    .unwrap();
    s.commit_items(None, &[survivor.as_str()], None).unwrap();
    s.complete_item(None, &survivor, CompleteInput::default())
        .unwrap();

    let unblocked: Vec<String> = s
        .next(None, None, None)
        .unwrap()
        .into_iter()
        .map(|i| i.reference)
        .collect();
    assert!(
        unblocked.contains(&dep),
        "dependent must dispatch once the survivor completes"
    );
}

/// HV-126: evolve supersede --with forwards the source's structural edges onto the
/// live replacement, so the replacement inherits the source's parent + dependents
/// and is not left orphaned.
#[test]
fn supersede_forwards_edges_onto_replacement() {
    let s = store();
    let parent = add(&s, "parent");
    let old = add(&s, "old approach");
    let newer = add(&s, "new approach");
    let dependent = ready_assigned(&s, "downstream", None);
    s.decompose(None, &parent.reference, &old.reference, false)
        .unwrap();
    s.depend(None, &dependent, &old.reference, false).unwrap(); // dependent depends on old

    s.evolve_supersede(None, &old.reference, &newer.reference, Some("redo"), None)
        .unwrap();

    let newer_edges = s
        .get_item(None, &newer.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        newer_edges.parents.contains(&parent.reference),
        "replacement inherits the source's decomposition parent"
    );
    assert!(
        newer_edges.blocks.contains(&dependent),
        "replacement inherits the source's dependent"
    );
    let old_edges = s
        .get_item(None, &old.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        old_edges.parents.is_empty() && old_edges.blocks.is_empty(),
        "superseded source keeps no structural edges"
    );
}

/// HV-126: superseding a group MEMBER forwards its inbound grouping onto the
/// replacement — the replacement joins the source's group, the source leaves it.
#[test]
fn supersede_forwards_inbound_grouping_to_replacement() {
    let s = store();
    let phase = container(&s, "phase", NodeType::Phase);
    let old = add(&s, "old member");
    let new = add(&s, "new member");
    s.group(None, &phase.reference, &old.reference, false)
        .unwrap();

    s.evolve_supersede(None, &old.reference, &new.reference, Some("redo"), None)
        .unwrap();

    let new_edges = s
        .get_item(None, &new.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        new_edges.groups.contains(&phase.reference),
        "replacement must join the source's group"
    );
    let old_edges = s
        .get_item(None, &old.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        old_edges.groups.is_empty(),
        "superseded member must leave the group"
    );
}

/// HV-126: superseding a CONTAINER that groups members with another container
/// forwards the outbound grouping — the replacement container inherits the members
/// (the survivor is a valid grouping target, so members are re-homed, not orphaned).
#[test]
fn supersede_forwards_outbound_grouping_to_container_survivor() {
    let s = store();
    let p1 = container(&s, "phase one", NodeType::Phase);
    let p2 = container(&s, "phase two", NodeType::Phase);
    let member = add(&s, "member");
    s.group(None, &p1.reference, &member.reference, false)
        .unwrap();

    s.evolve_supersede(
        None,
        &p1.reference,
        &p2.reference,
        Some("consolidate"),
        None,
    )
    .unwrap();

    let p2_edges = s
        .get_item(None, &p2.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        p2_edges.members.contains(&member.reference),
        "container replacement must inherit the source's members"
    );
    let member_edges = s
        .get_item(None, &member.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert_eq!(
        member_edges.groups,
        vec![p2.reference.clone()],
        "member is now grouped under the live replacement, not the superseded one"
    );
}

/// HV-126: re-homing a container's members onto a NON-container survivor is
/// impossible (a Task can't be a grouping target). Rather than silently orphan
/// them, the op is rejected and the whole transaction rolls back — no source is
/// superseded, no edge is dropped. Guards the merge case (the survivor is always a
/// Task) and supersede --with a non-container.
#[test]
fn evolve_rejects_grouping_member_orphan_into_noncontainer_survivor() {
    let s = store();
    let phase = container(&s, "phase", NodeType::Phase);
    let member = add(&s, "member");
    let other = add(&s, "other source");
    s.group(None, &phase.reference, &member.reference, false)
        .unwrap();

    // Merge the phase (with a member) + another node → a fresh Task survivor.
    let err = s
        .evolve_merge(
            None,
            &[phase.reference.clone(), other.reference.clone()],
            "merged",
            None,
            None,
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("orphaned"),
        "merge that would orphan a member must be rejected, got: {err}"
    );

    // Rolled back: the phase still groups the member and is NOT superseded.
    let phase_edges = s
        .get_item(None, &phase.reference, &[Include::Edges])
        .unwrap()
        .edges
        .unwrap();
    assert!(
        phase_edges.members.contains(&member.reference),
        "rejected merge must roll back — the member is still grouped"
    );
    assert_eq!(
        s.get_item(None, &phase.reference, &[]).unwrap().status,
        Status::Discovery,
        "rejected merge must not supersede the source"
    );

    // supersede --with a Task survivor is rejected the same way.
    let task = add(&s, "task survivor");
    assert!(
        s.evolve_supersede(None, &phase.reference, &task.reference, None, None)
            .is_err(),
        "supersede that would orphan a member must be rejected too"
    );
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
fn container_flags_uncommitted_descendants() {
    // The HV-59 shape: a container can read `done` while real work sits beneath it
    // as uncommitted floaters. `has_uncommitted_descendants` keeps that honest.
    let s = store();
    let phase = s
        .add_item(
            None,
            NewItem {
                title: "Track".into(),
                node_type: Some(NodeType::Phase),
                ..Default::default()
            },
        )
        .unwrap();
    let signals = |s: &Store| {
        let it = s.get_item(None, &phase.reference, &[]).unwrap();
        (it.rollup_state, it.has_uncommitted_descendants)
    };

    // Only an uncommitted floater → Dormant (the rollup ignores uncommitted work),
    // but the honesty flag fires.
    let floater = s
        .add_item(
            None,
            NewItem {
                title: "behavioral work, not yet committed".into(),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(signals(&s), (Some(RollupState::Dormant), Some(true)));

    // Add a committed, done child → the committed subtree is all-done so the
    // rollup reads `Done`, yet the floater keeps `has_uncommitted` true.
    let done_child = s
        .add_item(
            None,
            NewItem {
                title: "tooling leaf, shipped".into(),
                commit: true,
                status: Some(Status::Done),
                parent: Some(phase.reference.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(signals(&s), (Some(RollupState::Done), Some(true)));

    // project_graph agrees with get_item for the same container.
    let g = s.project_graph(None, false).unwrap();
    let node = g
        .nodes
        .iter()
        .find(|n| n.reference == phase.reference)
        .unwrap();
    assert_eq!(
        (node.rollup_state, node.has_uncommitted_descendants),
        (Some(RollupState::Done), Some(true))
    );

    // A leaf carries neither derived signal.
    let leaf = s.get_item(None, &done_child.reference, &[]).unwrap();
    assert_eq!(leaf.rollup_state, None);
    assert_eq!(leaf.has_uncommitted_descendants, None);

    // Dead (archived/superseded) descendants drop out of BOTH signals: archive the
    // floater and the flag clears, while the committed done child still yields Done.
    s.archive_item(None, &floater.reference, None, None)
        .unwrap();
    assert_eq!(signals(&s), (Some(RollupState::Done), Some(false)));
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

// ---- HV-67: due_at ------------------------------------------------------

#[test]
fn due_at_set_on_add_round_trips() {
    let s = store();
    let item = s
        .add_item(
            None,
            NewItem {
                title: "Ship it".into(),
                due_at: Some("2026-07-01".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(item.due_at.as_deref(), Some("2026-07-01"));
    // Re-read confirms it persisted (read path / ITEM_SELECT index 22).
    let read = s.get_item(None, &item.reference, &[]).unwrap();
    assert_eq!(read.due_at.as_deref(), Some("2026-07-01"));
}

#[test]
fn due_at_set_on_update_round_trips() {
    let s = store();
    let item = add(&s, "Plan launch");
    assert_eq!(item.due_at, None);
    let updated = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                due: Some(DueUpdate::Set("2024-02-29".into())), // a real leap day
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.due_at.as_deref(), Some("2024-02-29"));
}

#[test]
fn due_at_clear_via_none_sets_null() {
    let s = store();
    let item = s
        .add_item(
            None,
            NewItem {
                title: "Has a deadline".into(),
                due_at: Some("2026-12-31".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(item.due_at.as_deref(), Some("2026-12-31"));
    // Clearing maps to DueUpdate::Clear → due_at = NULL.
    let cleared = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                due: Some(DueUpdate::Clear),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(cleared.due_at, None);
}

#[test]
fn due_at_malformed_rejected_on_add_and_update() {
    let s = store();
    // add_item rejects a calendar-impossible-but-shaped date before any write.
    let err = s
        .add_item(
            None,
            NewItem {
                title: "Bad add".into(),
                due_at: Some("2026-13-01".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("invalid due_at"));

    // update_item rejects a malformed shape too.
    let item = add(&s, "Good item");
    let err = s
        .update_item(
            None,
            &item.reference,
            ItemUpdate {
                due: Some(DueUpdate::Set("2026/07/01".into())),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("invalid due_at"));
    // The rejected update left the column untouched (still NULL).
    let read = s.get_item(None, &item.reference, &[]).unwrap();
    assert_eq!(read.due_at, None);
}

#[test]
fn due_at_read_shape_full_carries_it_and_null_is_omitted() {
    let s = store();
    // With a value: serialized JSON carries the key.
    let with = s
        .add_item(
            None,
            NewItem {
                title: "Dated".into(),
                due_at: Some("2026-07-01".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let j = serde_json::to_value(&with).unwrap();
    assert_eq!(j["due_at"], serde_json::json!("2026-07-01"));

    // Without a value: the key is omitted entirely (skip_serializing_if).
    let without = add(&s, "Undated");
    let j = serde_json::to_value(&without).unwrap();
    assert!(
        j.get("due_at").is_none(),
        "null due_at must be omitted from the full read shape, got: {j}"
    );
}
