---
name: selachi-delegation
description: Delegate a self-contained subtask to another coding agent (Codex, Gemini, OpenCode, Claude, Antigravity) through Selachi's MCP tools (list_agents, run_agent, agent_status, agent_result, cancel_agent, get_diff, apply_changes). Use it for a second opinion, a cross-review of your own change, parallel mechanical work, or a cheaper model for bulk tasks. Do not use it for work that needs your conversation context, for secrets, or for a quick edit you can do yourself.
---

# Delegating to other agents with Selachi

Selachi runs other coding agents for you under limits: your permission is the ceiling, runs can live in
their own git worktree, every run has a timeout and a token budget, secrets are redacted, and anything
the other agent wants approved goes to the user in the Selachi window, never to you.

(ES: Selachi ejecuta otros agentes de código por ti con límites. Tu permiso es el techo, el trabajo puede
ir en un worktree aparte y los cambios solo entran si el usuario los confirma.)

## When to delegate

| Situation | Example | Settings |
|---|---|---|
| **Second opinion** on a design or a bug you are stuck on | "Here is the failing test and my theory; do you agree?" | `read_only` |
| **Cross-review** of your own change by a different model | "Review this diff for bugs and missing tests" (paste or describe it) | `read_only` |
| **Parallel mechanical work** that does not depend on each other | Rename a symbol across 40 files, add doc comments, port tests | `accept_edits` + `worktree: true`, one run per independent slice, `async: true` |
| **A cheaper model for bulk work** | Summarise 30 files, generate fixtures, boilerplate | `read_only` or `accept_edits` in a worktree |
| **A different tool's strength** | Another agent has a skill or access you lack | `read_only` first |

Do **not** delegate when: the task needs the context of this conversation and cannot be written down in a
paragraph; you could do it faster yourself; it touches credentials, tokens, `.env` files or private keys;
or the user asked you not to. A delegation costs real tokens on someone's plan.

## How to call the tools

1. **`list_agents`** first. It shows which agents are installed, whether delegation to each is allowed,
   their usage limits, and **your own limits** (`delegation.effective_max_permission`, depth, runs in
   flight, daily budget). Pick an agent that is installed and not near its weekly limit.
2. **`run_agent`** with `agent`, `prompt`, `cwd` (absolute project path) and:
   - `permission`: `read_only` (default) | `ask` | `accept_edits`. Never ask for more than you have.
     `full` does not exist for delegations.
   - `worktree: true` whenever the agent may edit files: your working tree stays untouched.
   - `async: true` for anything that may take more than a minute; then poll.
   - `timeout_s` / `budget_tokens` to bound it (defaults: 15 min, no token cap beyond the daily budget).
   - `model` only if you know the id (`list_agents` with `models: true`; Claude accepts `sonnet|opus|haiku`).
3. **Write a self-contained prompt**: the goal, the files or symbols involved, the constraints, and the
   output you want ("reply with a bullet list of problems, file:line first"). The other agent has none of
   your context. Ask for a concise answer.
4. **Blocking runs** return `{run_id, status, summary, files_changed, diff_stat, usage, cost, worktree_path}`.
   **Async runs** return `{run_id, status: "running"}`: poll `agent_status(run_id)`, then
   `agent_result(run_id)`. Poll sparingly (every 20 to 30 s at most); do other useful work in between.
   `cancel_agent(run_id)` stops a run you no longer need.
5. **Judge the result, do not trust it.** The summary is another model's claim. Read the code it
   changed with `get_diff(run_id)` (use `stat: true` first for big changes) and run the tests yourself
   before reporting success to the user.
6. **`apply_changes(run_id)`** only for worktree runs you have reviewed. It never applies silently: it
   returns the plan, and the user confirms in the Selachi window or with
   `selachi-agent apply <run_id>` in a terminal. Tell the user what is waiting for their confirmation and
   what it contains. Do not call it in a loop or try to work around the confirmation.

## Rules you must follow

- **Never put secrets in a prompt**: no API keys, tokens, passwords, connection strings, private keys,
  cookies, or the contents of `.env`/credential files. Redaction is a safety net, not permission.
  (ES: nunca delegues secretos.)
- **Respect refusals.** A refused call returns `isError: true` with a `code` and a reason. Act on it:
  - `permission_exceeds_parent` / `permission_full`: lower the permission; do not retry with the same one.
  - `depth_exceeded`: you are already a delegated agent; do the work yourself.
  - `concurrency`: wait for a run to finish (`agent_status`) or cancel one.
  - `daily_budget` / `weekly_usage`: pick another agent or tell the user; do not hammer it.
  - `agent_not_installed` / `agent_not_allowed` / `disabled`: tell the user, they can change it in
    Settings, Agents, Delegation.
  - `not_a_git_repo`: drop `worktree` or ask the user.
- **Prefer read-only.** Escalate to `accept_edits` only when edits are the point, and then in a worktree.
- **Do not chain delegations.** The delegated agent cannot delegate again by default.
- **Do not auto-confirm** anything on the user's behalf, and do not tell the user a change is applied
  until `agent_status` shows `applied: true`.
- **Keep the user informed**: say which agent you delegated to, why, what it cost (`usage`, `cost`),
  and what you verified. Quote the run id.
- A run that ends `timed_out`, `budget_exceeded` or `error` still has a `summary`/`reason`: read it
  before deciding whether to retry with a smaller task.

## Quick reference

```
list_agents({models?: bool})
run_agent({agent, prompt, cwd, model?, permission?: "read_only"|"ask"|"accept_edits",
           worktree?: bool, timeout_s?: int, budget_tokens?: int, async?: bool})
agent_status({run_id})     agent_result({run_id})     cancel_agent({run_id})
get_diff({run_id, stat?: bool})        apply_changes({run_id})   // plan only, user confirms
```

Fallback without MCP: the same engine from any shell, `selachi-agent run codex --cwd . --worktree
--permission read-only --json "<prompt>"` (see `selachi-agent --help`).
