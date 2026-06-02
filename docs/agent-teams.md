# Agent Teams

> **Experimental.** Agent Teams build on the multi-agent (`multi_agent_v2`)
> subsystem and are off by default. The shared task board and its tools are
> available once multi-agent v2 is enabled. A dedicated terminal panel for the
> board and a standalone config flag are still in progress.

Agent Teams let a *lead* session spawn independent *teammate* agents that
coordinate through a **shared task board** and message each other directly.
Unlike subagents (which only report back to the lead), teammates each run in
their own context window, claim work from a common backlog, and talk to one
another.

This page covers the shared task board and the tools that operate on it. For
spawning and messaging teammates, see the multi-agent tools (`spawn_agent`,
`send_message`, `followup_task`, `list_agents`).

## Enabling

Agent Teams ride the multi-agent v2 feature. In `config.toml`:

```toml
[features.multi_agent_v2]
enabled = true
```

When enabled, the lead and every teammate it spawns share **one task board**
for the lifetime of the team (the spawn subtree). A new top-level session starts
with an empty board.

## The shared task board

A task has a stable id, a title (and optional details), a status, an optional
assignee, and optional dependencies on other tasks:

| Field | Meaning |
| --- | --- |
| `status` | `pending` → `in_progress` → `completed` (or `cancelled`) |
| `assignee` | the teammate that claimed it; empty means unclaimed |
| `depends_on` | task ids that must be `completed` before this task is claimable |

A task is **claimable** only when it is `pending`, unclaimed, and all of its
dependencies are `completed`. Claiming is a race-free compare-and-set: if two
teammates try to claim the same task at once, exactly one wins.

## Tools

These tools are available to the lead and all teammates when the feature is on:

| Tool | Arguments | Effect |
| --- | --- | --- |
| `task_create` | `title`, `details?`, `depends_on?: [id]` | Add a task to the board. |
| `task_claim` | `task_id` | Claim a pending, unblocked task for yourself (CAS). |
| `task_update` | `task_id`, `status` | Change status. Completing a task unblocks its dependents. |
| `task_list` | — | Snapshot every task on the board. |
| `team_roster` | — | List the live teammates and their status. |

Title and details are bounded at the tool boundary (200 and 4096 chars) so the
board never injects unbounded text into a teammate's context.

## A typical flow

```text
Create a team to build a small feature. Spawn a researcher and an implementer.
Add tasks: "investigate API" and "write the client" (the client depends on the
investigation). Have each teammate claim and complete their work.
```

Under the hood: the lead calls `task_create` twice (the second `depends_on` the
first), spawns two teammates, and each teammate `task_claim`s an unblocked task,
does the work, and calls `task_update` with `completed`. Completing the
investigation automatically unblocks the client task, which the implementer can
then claim.

## Events

The board emits lifecycle events the UI can render: `TeamCreated`,
`TeamMemberJoined`, `TaskCreated`, `TaskUpdated`, and `TaskUnblocked`. These are
ephemeral (not persisted to the rollout). A dedicated terminal panel that renders
the live board is in progress.

## Limitations

- **In-process only.** The board is shared via an in-memory handle across the
  spawn subtree. Cross-process (`exec`) teams and an advisory file lock are not
  yet supported.
- **Resume.** The board is reloaded from the session, but teammates are not
  auto-restored across `/resume`; re-spawn them if needed.
- **One team per lead**, no nested teams, and the lead is fixed for the team's
  lifetime.

See the design docs for details: `docs/rfcs/0001-agent-teams.md` (design),
`docs/rfcs/0002-agent-teams-execution-plan.md` (build plan), and
`docs/rfcs/0003-agent-teams-squad-review.md` (review).
