# Squad Review 0003 — Investigating improvement opportunities (Agent Teams dogfood)

Status: **Findings — for triage**
Companions: RFC `0001-agent-teams.md`, execution plan `0002-...md`
Branch: `Horus`

---

## 1. How this was run (the test)

The goal was to **exercise the agent-teams workflow by having a squad investigate
improvement opportunities for the application**. The native Codex feature is still
mid-build (PR-1/PR-2 landed; see §4) and cannot be executed live here — running the
compiled Codex requires the full 122-crate workspace build, which is blocked in this
environment by a network-restricted git submodule (`libyuv`, pulled by the unrelated
`realtime-webrtc` crate), plus a live model endpoint.

So the squad was run on **Claude Code's own agent-team capability**, which mirrors
exactly the architecture being built: one **squad leader** (synthesis + coordination)
and three independent teammates, each with its own context window and a distinct lens:

| Teammate | Lens | Maps to the requested squad |
| --- | --- | --- |
| **Product** | feature gaps, adoption, competitiveness | "produto" |
| **UX Research** | terminal observability & steering | "UX research" |
| **Architect** | technical debt, correctness, design critique | "arquiteto" |
| **Lead** (this doc) | merge findings, resolve conflicts, prioritize | "squad leader" |

Each teammate investigated independently and grounded every finding in real files
(file:line). This document is the lead's synthesis: where they **agreed**, where they
**conflicted**, and the resulting priorities.

---

## 2. Strongest cross-cutting signal (all three agreed)

> **The capability is strong but invisible and illegible.** Codex already ships a
> mature multi-agent engine, yet there is (a) no shared task board, (b) no way for a
> user to *find* the feature, and (c) no live view of what agents are doing.

- **Product** and **UX** independently flagged **discoverability** as P1: the only
  related slash command is `MultiAgents` aliased to `subagents`, described merely as
  "switch the active agent thread" (`tui/src/slash_command.rs`); there is no `/team`
  entry point, no onboarding, and zero end-user docs (`docs/config.md` has no
  `multi_agent`/`collab` content).
- **UX** and **Architect** both warned that the planned `TaskBoardCell` "mirror
  `plans.rs`" is a trap: `PlanUpdateCell` is a **static append-only** history cell, so
  rendering a live, concurrently-updated board that way would spam the transcript and
  bury current state — and would not render `assignee`/`depends_on`, which are the
  board's whole point.

**Lead's call:** ship the board (PR-2 ✓) **with** a `/team` command + a *pinned/sticky*
board surface (not an append-only cell) + docs, as one coherent slice. A board nobody
can find or see repeats the current mistake.

---

## 3. Where the specialists conflicted (and the resolution)

- **"Reuse `plans.rs` styling" (execution plan PR-6) vs. UX/Architect objection.**
  The plan proposed reusing the plan-cell renderer; UX and Architect both pushed back.
  **Resolution:** keep the plan-cell *visual vocabulary* (checkbox glyphs, status
  colors) but render the board as a **single mutable pinned panel** keyed by `team_id`,
  re-rendered in place on `TaskUpdated`/`TaskUnblocked`, not appended per update.
  Update execution-plan PR-6 accordingly.

- **Product wants more teammates surfaced; Architect warns the registry isn't ready.**
  Product asked for richer roster/status UX; Architect showed `AgentRegistry` keeps
  `total_count` (atomic) and the agent tree (mutex) in **separate** sync, with O(n)
  scans (`registry.rs:80-181`), and no concurrency test. **Resolution:** before adding
  a `TeammateName → thread` lookup for the board (PR-4), add a reverse `HashMap` index
  and fold `total_count` into the mutex-guarded struct. Roster UX (Product) depends on
  this being correct first.

---

## 4. The Architect's critique of our own design — and what was already fixed

The architect reviewed RFC 0001/0002 adversarially and found real defects. Two were
**fixed during this session**:

1. **Claim-race story was contradictory (P0).** RFC §5.3 presented file locking as the
   shipping guarantee while the execution plan said in-process mutex. **Fixed:** §5.3
   reconciled — v1 is in-process-only; the in-memory `tokio::Mutex` provides the
   guarantee; file lock/exec/resume-restore explicitly deferred (Q4). The 32-way
   concurrent-claim test now backs this (PR-2).
2. **`persist` vs `persist_noclobber` (P0).** Confirmed only `persist_noclobber`
   (write-once) exists in the tree; a mutable board needs clobbering `persist`. **Fixed**
   in the RFC text; PR-2 keeps the board in-memory so persistence lands cleanly in PR-3.

Still **open** (folded into the roadmap, not yet fixed):

- **Resume/exec breaks the "shared `Arc` for free" backbone (P1).** On `/resume` the
  live `Arc` identity is lost; the in-process guarantee degrades to multi-writer-on-one-
  file. Documented in §5.3/§7 now; the real fix is the `FileLocked` backend gate.
- **`control.rs` is 1316 LoC (P0 debt)** vs. the AGENTS.md <500/<800 target. Routing
  Teams logic *into* it (PR-3/PR-4) makes it worse. **Recommendation:** extract
  spawn/fork/resume/rollout-filter into sibling modules under `agent/` *before* PR-3.
- **`TeammateIdle` derivation is ambiguous (P1).** `status.rs:is_final()` treats
  `Interrupted` as non-final; hooking "idle" naively risks re-prompt loops. Define
  `TeammateIdle` precisely on `Completed` with an empty mailbox, and rate-limit.
- **`wait_agent` can't wait on a task/teammate (P2).** It watches a payload-less
  `watch::Receiver<()>` and emits empty status vectors. Fold `Task*` events into the
  mailbox so the lead can wait on board changes instead of busy-polling.
- **Context-bounding is asserted, not enforced (P2).** Define task/roster injection as
  a concrete `ContextualUserFragment` in `core/context` with compile-time caps (an
  AGENTS.md review requirement), not a prose "≈200 chars".

---

## 5. Prioritized roadmap (lead's synthesis)

| Pri | Item | Source | Status |
| --- | --- | --- | --- |
| **P0** | Shared task board (in-process, CAS claim) | Product, Architect | ✅ PR-2 landed (logic verified) |
| **P0** | Reconcile claim-race + persistence story | Architect | ✅ fixed in RFC §5.3 |
| **P0** | Split `control.rs` before routing Teams through it | Architect | ⏳ pre-req for PR-3 |
| **P1** | `/team` command + first-run discoverability | Product, UX | ⏳ new, before/with PR-6 |
| **P1** | Pinned, mutable board panel (not append-only) | UX, Architect | ⏳ revises PR-6 |
| **P1** | Registry reverse-index + single-sync count | Architect | ⏳ before PR-4 |
| **P1** | Define `TeammateIdle` precisely + rate-limit | Architect | ⏳ in PR-5 |
| **P2** | `wait_agent` on task/teammate events | Architect | ⏳ extends mailbox |
| **P2** | `ContextualUserFragment` for task/roster | Architect | ⏳ in PR-4 |
| **P2** | End-user docs for multi-agent + roles | Product | ⏳ PR-7 |
| **P3** | Resume restores teammates from `TeamConfig` | Product | ⏳ needs FileLocked |
| **P3** | Remove dead `awaiter` role code | Architect | ⏳ cleanup |

---

## 6. Outcome

The dogfood run did what the feature is for: three independent agents surfaced
overlapping, conflicting, and complementary findings that a single pass would have
missed — most valuably, an **adversarial critique that caught two real defects in our
own RFC**, both fixed this session, plus a clear consensus that **discoverability and
live legibility** matter as much as the board itself. Those are now the top non-landed
priorities.
