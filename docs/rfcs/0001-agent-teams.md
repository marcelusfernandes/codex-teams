# RFC 0001 — Agent Teams

Status: **Draft / for review**
Author: (Horus branch)
Target crates: `codex-core`, `codex-protocol`, `codex-features`, `codex-app-server`, `codex-tui`

---

## 1. Summary

Add a first-class **Agent Teams** capability to Codex: a *team lead* session that
spawns independent *teammate* agents which (a) coordinate through a **shared task
list** with claiming, dependencies and conflict-free locking, and (b) talk to each
other directly through the existing inter-agent messaging.

The key insight from the design exploration is that **Codex already ships ~80% of
this feature** under the `multi_agent_v2` subsystem. This RFC deliberately does
*not* build a parallel system. It layers a thin **Team** abstraction and a **shared
task board** on top of the existing primitives, reusing spawn, messaging, mailbox,
roles, status tracking and the collab event stream verbatim.

This document is a design proposal only — no production code is implemented yet. It
exists so we can agree on scope, data model, tool surface, persistence and rollout
*before* writing the implementation, in keeping with `AGENTS.md` (small, reviewable
PRs; config-schema discipline; bounded context fragments).

---

## 2. Motivation

The reference behaviour we want to match is documented at
<https://code.claude.com/docs/en/agent-teams>. Compared to subagents (which only
report results back to one parent), agent teams let multiple independent agents:

- share a task list, claim work, and unblock dependent tasks automatically;
- message each other directly (not only the lead);
- be steered individually by the user;
- be governed by quality-gate hooks (`TaskCreated`, `TaskCompleted`, `TeammateIdle`).

Codex users already get most of this through `multi_agent_v2`, but there is **no
shared task board** and **no explicit notion of a "team"** — only ad-hoc
parent/child spawn trees and a per-agent `last_task_message` string. Those two gaps
are exactly what this RFC fills.

---

## 3. What already exists (reuse, do not rebuild)

Verified against the current tree on this branch:

| Capability | Where it lives today |
| --- | --- |
| Spawn independent agent (own thread/context) | `core/src/tools/handlers/multi_agents_v2/spawn.rs`; `AgentControl::spawn_agent_with_metadata` (`core/src/agent/control.rs`) |
| Direct teammate→teammate messaging | `multi_agents_v2/send_message.rs` + `message_tool.rs`; `AgentControl::send_inter_agent_communication` |
| Wake a teammate now vs. queue | `MessageDeliveryMode::{TriggerTurn,QueueOnly}` → `InterAgentCommunication.trigger_turn` (`protocol/src/protocol.rs`) |
| Mailbox / automatic delivery | `core/src/session/input_queue.rs` (`InputQueue`, delivery phases) |
| List live teammates | `multi_agents_v2/list_agents.rs`; `AgentControl::list_agents` |
| Shut down a teammate | `multi_agents_v2/close_agent.rs` |
| Reusable roles (a la subagent definitions) | `core/src/agent/role.rs`, `core/src/config/agent_roles.rs`, `config/agents/` |
| Status (pending/running/completed/…) | `core/src/agent/status.rs`; `watch::Receiver<AgentStatus>` |
| Hierarchical naming / addressing | `codex_protocol::AgentPath` |
| Lifecycle events for the UI | `Collab*` events (`CollabAgentSpawnBegin/End`, `CollabAgentInteractionBegin/End`, …) |
| Per-agent registry, spawn limits, RAII slots | `core/src/agent/registry.rs` (`AgentRegistry`, `SpawnReservation`) |
| Feature gating | `codex-features` (`Feature::MultiAgentV2`, `Feature::Collab`), wired in `core/src/tools/spec_plan.rs` |
| Tool registration pattern | `ToolExecutor<ToolInvocation>` + `CoreToolRuntime`; assembled in `spec_plan.rs::add_tool_sources` |

### 3.1 Mapping to the reference doc

| Agent Teams concept (doc) | Codex equivalent | Gap? |
| --- | --- | --- |
| Team lead | Root session in a spawn tree (`AgentPath::root`) | Needs a `Team` handle |
| Teammates | `multi_agent_v2` spawned agents | — |
| Mailbox | `InputQueue` | — |
| Task list (claim, deps, locking) | *only* `last_task_message` | **Build** |
| `~/.claude/teams/{name}/config.json` | none | **Build** (`<codex_home>/teams/…`) |
| `~/.claude/tasks/{name}/` | none | **Build** (`<codex_home>/tasks/…`) |
| `TaskCreated`/`TaskCompleted`/`TeammateIdle` hooks | `CodexHooks` exists; no team hook points | **Build** (hook points only) |
| Plan approval before implementing | spawn-time plan mode exists per-agent | minor wiring |

---

## 4. Goals / Non-goals

**Goals**

1. A **shared task board** scoped to a team: tasks with `pending → in_progress →
   completed` states, dependency edges, single-owner claiming, and concurrency-safe
   updates via file locking.
2. A **Team** abstraction: named teammates, team config persisted under
   `<codex_home>/teams/{team_id}/`, discoverable by all members.
3. New model-visible tools so the lead and teammates can create/claim/complete/list
   tasks and address each other by stable team-local names.
4. App-server + protocol surface so the TUI can render the board and team roster.
5. Feature-gated (`Feature::AgentTeams`), off by default, mirroring the experimental
   posture of the reference implementation.

**Non-goals (this RFC)**

- Split-pane / tmux UI orchestration (Codex TUI differs from Claude Code; out of scope).
- Nested teams (teammates spawning their own teams) — explicitly unsupported, as in
  the reference.
- Promoting/transferring leadership.
- Cross-machine / networked teams.

---

## 5. Design

### 5.1 Layering

```
                 ┌─────────────────────────────────────────────┐
   NEW           │  Team tools: task_create / task_claim /      │
   (this RFC)    │  task_update / task_list / team_roster       │
                 └───────────────┬─────────────────────────────┘
                                 │ uses
                 ┌───────────────▼─────────────────────────────┐
   NEW           │  TeamControl  +  TaskBoard (file-locked)     │
                 └───────────────┬─────────────────────────────┘
                                 │ built on
   EXISTING      │  AgentControl · InputQueue · AgentRegistry · │
                 │  InterAgentCommunication · Collab* events    │
                 └──────────────────────────────────────────────┘
```

`TeamControl` lives next to `AgentControl` in `SessionServices` and holds a handle to
the team's `TaskBoard`. A team is *implicitly* the spawn subtree rooted at the lead;
`TeamControl` simply gives that subtree an id, a roster, and a board.

### 5.2 Data model (`codex-protocol`)

New module `protocol/src/team.rs`, re-exported from the protocol crate:

```rust
/// Stable, team-local identifier for a teammate (e.g. "researcher", "reviewer-1").
/// Distinct from AgentPath (which is structural). The lead assigns these.
pub struct TeammateName(String);

pub struct TeamId(String);          // ULID-ish; used in on-disk paths

pub enum TaskStatus { Pending, InProgress, Completed, Cancelled }

pub struct TaskId(String);          // ULID

pub struct Task {
    pub id: TaskId,
    pub title: String,              // bounded; see §5.6
    pub details: Option<String>,    // bounded
    pub status: TaskStatus,
    pub assignee: Option<TeammateName>,   // None = unclaimed
    pub depends_on: Vec<TaskId>,    // task is blocked until all are Completed
    pub created_by: TeammateName,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

pub struct TeamMember {
    pub name: TeammateName,
    pub agent_path: AgentPath,      // bridges to existing addressing
    pub agent_id: ThreadId,
    pub role: Option<String>,       // existing role name
}

pub struct TeamConfig {
    pub id: TeamId,
    pub name: String,
    pub lead_thread_id: ThreadId,
    pub members: Vec<TeamMember>,   // runtime state; regenerated, never hand-edited
}
```

A pending task is **claimable** iff `assignee.is_none()` and every `depends_on` task
is `Completed`. When a task transitions to `Completed`, the board recomputes which
dependents are now unblocked and emits events (see §5.5).

### 5.3 Persistence & concurrency (the crux)

Mirror the reference layout under `codex_home` (resolved exactly like other Codex
config; see `config/mod.rs::codex_home`):

```
<codex_home>/teams/{team_id}/config.json     # TeamConfig (runtime state)
<codex_home>/tasks/{team_id}/board.json       # the task list
<codex_home>/tasks/{team_id}/board.lock       # advisory lock file
```

Because teammates are separate async tasks within the same process today (and could
be separate processes for `exec`), claims must be race-free. The board uses an
**advisory file lock** (`fs2`/`fd-lock` style — pick one already in the tree;
otherwise an OS `flock`) around a read-modify-write cycle:

```
lock(board.lock) → read board.json → mutate → atomic write (tmp + rename) → unlock
```

This is the same "task claiming uses file locking to prevent race conditions"
guarantee the reference describes. `task_claim` is the only contended op and is
implemented as compare-and-set under the lock: claim succeeds only if the task is
still `Pending && assignee.is_none()`.

> Open question (Q1, §9): in-process teammates could instead share an
> `Arc<Mutex<TaskBoard>>` and skip files entirely, with the file layer used only for
> persistence/resume. Cross-process `exec` teams need the file lock. The proposal is
> to implement **both behind one `TaskBoard` trait** (`InProcess` and `FileLocked`),
> selected by session source.

### 5.4 New tools (model-visible)

Each follows the verified pattern: a handler implementing
`ToolExecutor<ToolInvocation> + CoreToolRuntime`, a `ToolSpec::Function` built with
`JsonSchema::object(...)`, registered in `spec_plan.rs` behind the feature gate.
Proposed namespace: reuse the collab grouping so they appear alongside
`spawn_agent`/`send_message`.

| Tool | Args | Behaviour |
| --- | --- | --- |
| `task_create` | `title`, `details?`, `depends_on?: [task_id]`, `assignee?: name` | Append task; fire `TaskCreated` hook (may block). |
| `task_claim` | `task_id` | CAS-claim under lock; errors if blocked/taken. |
| `task_update` | `task_id`, `status`, `note?` | Transition; on `completed` fire `TaskCompleted` hook and unblock dependents. |
| `task_list` | `filter?: {status, assignee}` | Read board (no lock needed for read). |
| `team_roster` | — | Return `TeamConfig.members` so any teammate can discover the others (reference: "teammates can read this file to discover other team members"). |

Addressing for messaging stays on the **existing** `send_message` / `followup_task`
tools; we extend target resolution (`resolve_agent_target`) to accept a
`TeammateName` in addition to `AgentPath`/nickname/thread-id. No new messaging tool.

`spawn_agent` gains optional `teammate_name` so the lead can give predictable names
(reference: "tell the lead what to call each teammate"). If omitted we fall back to
the existing nickname generator.

### 5.5 Events (`codex-protocol` + app-server)

Add team/task lifecycle events alongside the existing `Collab*` family so the TUI
can render a live board without polling:

- `TeamCreatedEvent { team_id, name }`
- `TeamMemberJoinedEvent { team_id, member }`
- `TaskCreatedEvent { team_id, task }`
- `TaskUpdatedEvent { team_id, task_id, status, assignee }`
- `TaskUnblockedEvent { team_id, task_id }`

These reuse the same `EventMsg` plumbing as `PlanUpdate`
(`handlers/plan.rs` shows the one-liner `session.send_event(...)` pattern). App-server
exposes the board snapshot via a new read method; TUI renders a task panel (can reuse
the existing plan/`update_plan` widget styling).

### 5.6 Context discipline (`AGENTS.md`)

Per `AGENTS.md`, anything injected into a model context must be a bounded
`ContextualUserFragment`. Teammates must *not* receive the entire board on every
turn. Plan:

- Cap `Task.title` (e.g. 200 chars) and `Task.details` (e.g. a few KB) at the tool
  boundary, rejecting oversized input back to the model.
- When notifying a teammate of an assignment, inject a single bounded fragment
  (the one task), not the whole board. The board is pulled on demand via `task_list`.
- Roster injection is bounded by team size (cap teammates, see §7).

### 5.7 Hooks (`Feature::CodexHooks`)

Add three hook points, firing through the existing hooks pipeline used by
`pre_tool_use`/`post_tool_use`:

- `TaskCreated` — before a task is persisted; non-zero/blocking result cancels creation.
- `TaskCompleted` — before marking complete; can reject to keep it open.
- `TeammateIdle` — when a teammate is about to go idle (derived from the existing
  `AgentStatus → Completed/Interrupted` transition in `agent/status.rs`); blocking
  result re-prompts the teammate.

Only the *hook points* are in scope here; authoring UX matches existing hooks.

---

## 6. Feature gating & config

- New `Feature::AgentTeams` in `codex-features` (`lib.rs`), experimental, default off.
  Enabling it implies `MultiAgentV2` (same self-enable trick already used for
  `SpawnCsv → Collab` at `features/src/lib.rs:515`).
- New `[features].agent_teams` toggle + an `[agent_teams]` config block
  (`max_teammates`, `max_tasks`, `board_backend = "auto|in_process|file"`). Any new
  `ConfigToml` field must update the config schema and `docs/config.md`
  (`AGENTS.md` requirement). `example-config.md` updated too.

---

## 7. Limitations (carried over intentionally)

Same caveats the reference flags, plus Codex specifics:

- One team per lead; clean up before creating another.
- No nested teams; only the lead manages membership.
- Lead is fixed for the team's lifetime.
- Resume: teammates are not auto-restored across `/resume`; board *is* restored from
  disk, but the lead may need to re-spawn teammates (reuse existing
  `resume_agent_from_rollout`).
- Recommended team size 3–5 (enforced softly via `max_teammates`).

---

## 8. Phased implementation plan (small, reviewable PRs)

Each phase is independently mergeable and behind the feature flag.

1. **PR-1 — Protocol & feature flag.** Add `protocol/src/team.rs` types, the
   `Team*`/`Task*` events, and `Feature::AgentTeams`. No behaviour yet. Pure additive.
2. **PR-2 — `TaskBoard` core.** `TaskBoard` trait + `InProcess` impl + `FileLocked`
   impl with the lock/CAS-claim logic and atomic writes. Unit tests covering claim
   races and dependency unblocking. No model surface yet.
3. **PR-3 — `TeamControl` + persistence.** Wire `TeamControl` into `SessionServices`;
   create/load `teams/{id}/config.json`; roster discovery.
4. **PR-4 — Tools.** `task_create/claim/update/list`, `team_roster`; extend
   `resolve_agent_target` for `TeammateName`; `spawn_agent.teammate_name`. Register
   in `spec_plan.rs` behind the gate.
5. **PR-5 — Hooks.** `TaskCreated`/`TaskCompleted`/`TeammateIdle` points.
6. **PR-6 — App-server + TUI.** Board snapshot read method; task panel rendering;
   roster view.
7. **PR-7 — Docs & config schema.** `docs/config.md`, `example-config.md`, a user
   guide modeled on the reference page, schema regen.

Testing throughout uses `just test -p <crate>` for scoped crates and `just fmt` /
`just fix` before finalizing, per `AGENTS.md`. Never `cargo test` directly.

---

## 9. Open questions

- **Q1 — Board backend.** In-process `Arc<Mutex<…>>` vs. always file-locked. Proposal:
  trait with both, auto-selected by session source. Confirm.
- **Q2 — Reuse `update_plan`?** The existing `update_plan` tool already models a
  per-agent step list. Should the team board *subsume* it for teammates, or stay
  separate? Proposal: keep separate (per-agent plan vs. shared team board) to avoid
  semantic overload.
- **Q3 — Namespace.** Put new tools under the existing collab namespace or a new
  `team` namespace? Proposal: collab namespace, for discoverability next to
  `spawn_agent`.
- **Q4 — `exec` (headless) teams.** Do we support team mode in `codex exec`, or
  TUI/app-server only first? Proposal: design board to be process-safe so `exec`
  works later, but ship UI surface first.

---

## 10. Appendix — reference tool skeleton

Concrete shape PR-4 will follow (verified against `handlers/plan.rs` and
`multi_agents_v2/message_tool.rs`):

```rust
pub(crate) struct TaskClaimHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for TaskClaimHandler {
    fn tool_name(&self) -> ToolName { ToolName::plain("task_claim") }
    fn spec(&self) -> ToolSpec { create_task_claim_tool() }

    async fn handle(&self, invocation: ToolInvocation)
        -> Result<Box<dyn ToolOutput>, FunctionCallError>
    {
        let ToolInvocation { session, turn, payload, .. } = invocation;
        let args: TaskClaimArgs = parse_arguments(&function_arguments(payload)?)?;
        let me = turn.session_source.get_agent_path()
            .and_then(teammate_name_for_path)
            .ok_or_else(|| FunctionCallError::RespondToModel(
                "only teammates can claim tasks".into()))?;
        let task = session.services.team_control
            .board()
            .claim(args.task_id, me)            // lock + CAS inside
            .await
            .map_err(|e| FunctionCallError::RespondToModel(e.to_string()))?;
        session.send_event(&turn,
            TaskUpdatedEvent { /* … */ }.into()).await;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            serde_json::to_string(&task).unwrap_or_default(), Some(true))))
    }
}

impl CoreToolRuntime for TaskClaimHandler {}
```
