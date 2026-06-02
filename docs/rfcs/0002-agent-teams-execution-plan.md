# Execution Plan 0002 — Building Agent Teams

Status: **Ready to execute**
Companion to: `docs/rfcs/0001-agent-teams.md`
Branch: `Horus`

---

## 1. Context

RFC 0001 proposes a native **Agent Teams** capability layered on the existing
`multi_agent_v2` subsystem: a shared, conflict-free **task board** plus a **Team**
abstraction reusing spawn/messaging/mailbox/roles/status. This document is the
*execution* plan — how to build it correctly and efficiently, with exact files,
reused helpers, a parallelization strategy using subagents/agent-teams, and
verification steps.

All integration points below were verified against the tree on the `Horus` branch.

### Key architectural finding (de-risks the whole design)

`AgentControl` is `#[derive(Clone)]` and holds `state: Arc<AgentRegistry>`. When a
teammate is spawned, `CodexSpawnArgs` does `agent_control: parent.services.agent_control.clone()`
(`core/src/codex_delegate.rs:79-106`), so the **same `Arc` state is shared across the
entire spawn subtree**. Therefore a `TeamControl` built the same way — `Clone`, holding
`Arc<SharedTeamState>` — gives every teammate a handle to the **same task board for
free**, with no extra plumbing. This is the backbone of the design.

---

## 2. Two corrections to fold in (vs. the raw exploration notes)

1. **Atomic write of a *mutable* file.** The board is rewritten repeatedly, so
   `persist_noclobber` (write-once, used by rollout compression) is **wrong** here.
   Use `tempfile::Builder::new().tempfile_in(dir)` → write → `sync_all()` →
   `temp.persist(path)` (clobbering) — i.e. atomic tmp+rename. `tempfile` is already a
   workspace dep (`codex-rs/Cargo.toml:390`); no new dependency.
2. **Atomic rename does NOT prevent the read-modify-write race.** Two writers can both
   read v1 and the last rename wins. Concurrency is solved per backend:
   - **In-process backend** (default): an `Arc<Mutex<TaskBoardInner>>` inside the shared
     `TeamControl` serializes the read-modify-write. The JSON file is just durable
     persistence written under the same lock.
   - **Cross-process backend** (`exec`, deferred): needs a real advisory lock
     (`flock`/`fs4`). Out of scope for the first ship (RFC Q4); the trait leaves room.

---

## 3. Dependency graph of the work

```
PR-1 Protocol types + events + Feature flag   ← foundation, blocks everything
   │
   ├──► PR-2 TaskBoard core (trait + InProcess + tests)      ─┐
   │                                                          │
   ├──► PR-3 TeamControl + persistence + wiring  ◄────────────┘ (needs PR-2 types)
   │        │
   │        └──► PR-4 Model-visible tools (task_*, team_roster, spawn name) ◄─ needs PR-3
   │
   ├──► PR-6 TUI panel + app-server forwarding  (needs PR-1 events only)
   │
   └──► PR-7 Docs + config schema  (needs PR-1 config block)

PR-5 Hooks (TaskCreated/Completed/TeammateIdle)  ← needs PR-4 (tool hook points) + PR-3
```

Critical path: **PR-1 → PR-3 → PR-4 → PR-5**. PR-2, PR-6, PR-7 fan out off PR-1.

---

## 4. How to build it with agent teams / subagents (efficiency + quality)

The build is itself a textbook agent-teams workload: independent modules, different
files, clear deliverables. Recommended orchestration:

**Wave 0 (sequential, single agent — the foundation):** Implement **PR-1** alone.
Everything imports these types/events/flag, so doing it first avoids merge churn.
Land it (compiles, `just fmt`) before fanning out.

**Wave 1 (parallel team, 3 teammates on disjoint files):**
- **Teammate A → PR-2 `TaskBoard` core.** Files under a new `core/src/team/` module.
  Owns the trait, `InProcess` impl, claim/dependency logic, and unit tests. Touches no
  shared files. Highest-risk logic → give it plan-approval before coding.
- **Teammate B → PR-6 TUI + app-server forwarding.** Files under `tui/src/` and
  `app-server/`. Depends only on PR-1 events. Disjoint from A.
- **Teammate C → PR-7 docs + config schema.** `docs/`, `example-config.md`, schema
  regen. Disjoint from A and B.

**Wave 2 (sequential, single agent — the integration seam):** **PR-3 TeamControl
wiring** touches `state/service.rs`, `session/session.rs`, `thread_manager.rs`,
`codex_delegate.rs` — high-contention shared files, so do it solo after PR-2 lands to
avoid conflicts. Then **PR-4 tools** (depends on PR-3), then **PR-5 hooks**.

**Coordination mechanics to use (all already in the repo):**
- Shared task list = the GitHub PR checklist + the RFC phases; one task per PR.
- `spawn_agent` with `agent_type` set to a reviewer role for parallel code-review of
  each PR (mirrors the "parallel code review" use case).
- `send_message`/`followup_task` between teammates when an interface changes (e.g. A
  finalizes `TaskBoard` signatures → messages the integrator before PR-3).
- Run `just fmt && just fix && just clippy` as a per-teammate quality gate before each
  hand-off.

> Note: agent teams cost significantly more tokens. Use the team for Wave 1 (genuinely
> parallel); use a single agent for Waves 0 and 2 (sequential seams). Don't parallelize
> work that edits the same files.

---

## 5. Per-PR execution detail

### PR-1 — Protocol, events, feature flag (foundation)
- **New file** `protocol/src/team.rs`: `TeammateName`, `TeamId`, `TaskId`, `TaskStatus`,
  `Task`, `TeamMember`, `TeamConfig`. Derive `Serialize, Deserialize, JsonSchema, TS`
  (mirror `protocol/src/plan_tool.rs:22-29`). Export from `protocol/src/lib.rs`
  (`pub mod team;` + `pub use team::...`, alongside `AgentPath` at lines 1-32).
- **Events** in `protocol/src/protocol.rs`: add `EventMsg` variants
  `TeamCreated/TeamMemberJoined/TaskCreated/TaskUpdated/TaskUnblocked` + payload structs
  + `From<…> for EventMsg` impls (copy the `Collab*` pattern at
  `protocol.rs:1510-1566` and `:3722-3734`).
- **Feature flag** in `features/src/lib.rs`: add `Feature::AgentTeams`; add a
  `FeatureSpec { id, key: "agent_teams", stage: UnderDevelopment, default_enabled: false }`
  to `FEATURES` (`:948-965`); add the self-enable rule in `normalize_dependencies()`
  (`:514-521`): `if AgentTeams && !MultiAgentV2 { enable(MultiAgentV2) }`.
- **Config block**: add `AgentTeamsConfigToml` in `features/src/feature_configs.rs`
  (mirror `MultiAgentV2ConfigToml`) with `max_teammates`, `max_tasks`,
  `board_backend`; wire into `FeaturesToml` (`:601-611`).
- *Acceptance:* workspace compiles; new variants serialize; no behaviour change.

### PR-2 — `TaskBoard` core (parallelizable)
- **New module** `core/src/team/board.rs`:
  ```rust
  trait TaskBoard {
      async fn create(&self, t: NewTask) -> Result<Task>;
      async fn claim(&self, id: TaskId, who: TeammateName) -> Result<Task>; // CAS
      async fn update(&self, id: TaskId, status: TaskStatus, note: Option<String>) -> Result<Task>;
      async fn list(&self, filter: TaskFilter) -> Vec<Task>;
  }
  ```
- `InProcessBoard { inner: Arc<Mutex<BoardInner>>, persist_path: PathBuf }`. All mutating
  ops lock the mutex, mutate, then **atomic-write** via the `tempfile + persist`
  pattern from `rollout/src/compression.rs:609-627` (but `persist`, not
  `persist_noclobber`).
- `claim` = compare-and-set: succeed only if `Pending && assignee.is_none() && deps all Completed`.
- `update`→`Completed` recomputes unblocked dependents and returns them so the caller
  can emit `TaskUnblocked` events.
- **Tests** (`just test -p codex-core`): concurrent claim (only one wins), dependency
  gating, unblock cascade, persistence round-trip.

### PR-3 — `TeamControl` + wiring (integration seam, solo)
- **New** `core/src/team/control.rs`: `TeamControl { state: Arc<SharedTeamState> }`,
  `#[derive(Clone, Default)]`, holding the board + `TeamConfig`. Construct a sibling to
  `thread_manager.rs:920` (`fn agent_control()`), e.g. `fn team_control()`.
- Add field `team_control: TeamControl` to `SessionServices`
  (`core/src/state/service.rs:69`, next to `agent_control`); construct it in
  `session.rs` (~`:976-1049`) and thread it through `Session::new` and
  `CodexSpawnArgs` (`codex_delegate.rs:79-106`) with `.clone()` so teammates share it.
- Persistence: `<codex_home>/teams/{id}/config.json` + `<codex_home>/tasks/{id}/board.json`
  using the `AbsolutePathBuf::join` + `create_dir_all` pattern from
  `core-plugins/src/store.rs:17-45`. `codex_home` is available via config at
  construction time.

### PR-4 — Model-visible tools (needs PR-3)
- **New** handlers under `core/src/tools/handlers/` (mirror
  `handlers/plan.rs` + `multi_agents_v2/message_tool.rs`): `task_create`, `task_claim`,
  `task_update`, `task_list`, `team_roster`. Each = `ToolExecutor<ToolInvocation> +
  CoreToolRuntime`, spec via `JsonSchema::object(...)`, reaches
  `session.services.team_control`.
- Extend `resolve_agent_target` (multi_agents_v2) to accept a `TeammateName`.
- Add optional `teammate_name` arg to `spawn_agent` (`multi_agents_v2/spawn.rs:243`).
- Register all in `spec_plan.rs` behind `Feature::AgentTeams` (add an
  `add_agent_teams_tools` called from `add_tool_sources`, mirroring
  `add_collaboration_tools`).
- Enforce **context bounds** (RFC §5.6): cap title/details at the tool boundary.

### PR-5 — Hooks (needs PR-4)
- Add `TaskCreated`/`TaskCompleted` hook points inside `task_create`/`task_update`
  (reuse the pre/post hook plumbing from `tools/registry.rs`), and `TeammateIdle`
  derived from the `AgentStatus → Completed/Interrupted` transition in
  `core/src/agent/status.rs`. Blocking (exit-2) result re-prompts / cancels.

### PR-6 — TUI + app-server (needs PR-1 events only; parallelizable)
- App-server: add new `EventMsg` arms to the auto-forward match in
  `app-server/src/bespoke_event_handling.rs:829-849`, and map them in
  `app-server-protocol/src/protocol/event_mapping.rs` to `ServerNotification`s.
- TUI: new `TaskBoardCell` in `tui/src/history_cell/` (mirror `plans.rs:160-238`);
  handle the new notification in `tui/src/chatwidget/protocol.rs:98-114`; add
  `on_task_board_update()` in `chatwidget/turn_runtime.rs` (mirror `on_plan_update`).

### PR-7 — Docs + config schema (parallelizable)
- New user guide `docs/agent-teams.md` (model on the reference page).
- Update `docs/config.md`, `example-config.md`, regenerate config schema
  (`AGENTS.md` requires schema update on `ConfigToml` changes).

---

## 6. Verification (end-to-end)

Per `AGENTS.md`: always use `just`, never `cargo` directly. Before each hand-off:

```
just fmt
just fix
just clippy
just test -p codex-protocol      # PR-1
just test -p codex-core          # PR-2, PR-3, PR-4, PR-5
just test -p codex-tui           # PR-6
just test                        # only before the final integration PR
```

Manual smoke test (after PR-4):
1. Enable in `config.toml`: `[features] agent_teams = true`.
2. `just codex` → ask the lead to "create a team with 2 teammates, add 3 tasks with a
   dependency, and have teammates claim and complete them."
3. Confirm: tasks appear in the TUI board panel; only one teammate claims each task;
   completing a blocker unblocks its dependent; `team_roster` lists members.
4. Inspect `<codex_home>/teams/<id>/config.json` and `<codex_home>/tasks/<id>/board.json`.

Concurrency test (automated, PR-2): spawn N tokio tasks racing `claim` on one task;
assert exactly one `Ok` and N-1 `Err`.

---

## 7. Risks & mitigations

| Risk | Mitigation |
| --- | --- |
| Race on claim | `Arc<Mutex>` CAS in-process; advisory lock deferred for `exec` |
| Merge conflicts on shared files (service.rs/session.rs) | PR-3 done solo in Wave 2, not in the parallel team |
| Context bloat into teammates | Bound fragments at tool boundary (RFC §5.6) |
| Schema drift | PR-7 regenerates schema; CI `argument-comment-lint`/schema checks catch it |
| Scope creep into exec/nested teams | Explicitly out of scope (RFC §7); trait leaves seams |
| Token cost of team-based build | Team only for Wave 1; single agent for sequential seams |

---

## 8. Open questions carried from RFC 0001

Q1 board backend (in-process vs file) · Q2 reuse `update_plan`? · Q3 tool namespace ·
Q4 `exec` headless support. Recommended defaults are in RFC §9; confirm before PR-2.
