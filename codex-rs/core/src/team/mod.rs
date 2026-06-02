//! Agent Teams: the shared, conflict-free task board.
//!
//! A team is the spawn subtree rooted at the lead. This module owns the
//! coordination primitive that the existing `multi_agent_v2` subsystem lacks:
//! a shared task board where teammates claim work, with dependency gating and
//! a compare-and-set claim that is safe under concurrent access.
//!
//! Layering (see `docs/rfcs/0002-agent-teams-execution-plan.md`):
//!   * PR-2 (this change): the in-process [`TaskBoard`] core + tests.
//!   * PR-3: `TeamControl` carries a cloned `TaskBoard` through `SessionServices`
//!     (mirroring how `AgentControl` shares its `Arc` registry across the spawn
//!     subtree) and owns on-disk persistence.
//!   * PR-4: model-visible `task_*` / `team_roster` tools call into this board.
#![allow(dead_code)] // Staged landing: wired into SessionServices in PR-3.

pub(crate) mod board;
