//! `haven-core` — the Haven work-graph service layer.
//!
//! One `Store` owns the local SQLite connection and the `~/.haven` content
//! root. Every domain operation is a method on `Store`; the CLI and the MCP
//! server both call these identical methods, so behaviour cannot drift between
//! them (the Muxra shared-service-layer pattern, SPEC §7).
//!
//! Layering so far: `db` (connection + migrations) and `error`. Domain types
//! and the `Store` service land in Layer 2.

pub mod db;
pub mod error;
pub mod model;
pub mod sortkey;
pub mod store;
pub mod telemetry;
mod time;
mod util;

pub use error::{HavenError, Result};
pub use model::*;
pub use store::{
    AddOutcome, ArtifactContent, BackupEntry, BackupReport, CompleteInput, CompleteResult,
    DispatchArtifact, DispatchCandidate, DispatchContextItem, DispatchRecommendation,
    DispatchSummary, DueUpdate, EdgeKind, EvolveResult, GraphEdge, HandoffInput, HandoffResult,
    ImportItem, ImportOutcome, Include, Integrity, ItemFilter, ItemUpdate, LineageDirection,
    LineageGraph, LineageLink, NewArtifact, NewItem, Prime, PrimeActiveItem, PrimeInboxItem,
    PrimeQueueItem, ProjectArchive, ProjectGraph, RestoreReport, SimilarItem, StaleRef, Store,
    WaitUpdate, DEFAULT_DISPATCH_LIMIT, DEFAULT_NEXT_LIMIT,
};
