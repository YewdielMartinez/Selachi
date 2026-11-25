---
name: selachi-delegation
description: Delegate a self-contained subtask to another coding agent (Codex, Gemini, OpenCode, Claude, Antigravity) through Selachi's MCP tools (list_agents, list_profiles, run_agent, agent_status, agent_result, cancel_agent, get_diff, apply_changes). Use it for a second opinion, a cross-review of your own change, parallel mechanical work, or a cheaper model for bulk tasks. Do not use it for work that needs your conversation context, for secrets, or for a quick edit you can do yourself.
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
2. **`list_profiles`** (pass `cwd` to include the project's own profiles). A *profile* is a named
   specialization someone set up (for example "Revisor de seguridad", "Tests", "Docs", "Investigador"):
   it brings its own agent, model, effort, instructions, context files and limits. Read each
   `description`, and when one fits the task, **use it** instead of choosing everything by hand. If
   none fits, or the list is empty, call `run_agent` with an explicit `agent`. See "Profiles" below.
3. **`run_agent`** with `profile` (or `agent`), `prompt`, `cwd` (absolute project path) and:
   - `permission`: `read_only` (default) | `ask` | `accept_edits`. Never ask for more than you have.
     `full` does not exist for delegations.
   - `worktree: true` whenever the agent may edit files: your working tree stays untouched.
   - `async: true` for anything that may take more than a minute; then poll.
   - `timeout_s` / `budget_tokens` to bound it (defaults: 15 min, no token cap beyond the daily budget).
   - `approvals`: `ask` | `allowlist` | `auto`, how the child's own commands are approved (see "Approvals").
     Leave it out unless you want it stricter than the user's setting.
   - `model` only if you know the id (`list_agents` with `models: true`; Claude accepts `sonnet|opus|haiku`).
4. **Write a self-contained prompt**: the goal, the files or symbols involved, the constraints, and the
   output you want ("reply with a bullet list of problems, file:line first"). The other agent has none of
   your context. Ask for a concise answer.
5. **Blocking runs** return `{run_id, status, summary, files_changed, diff_stat, usage, cost, worktree_path}`.
   **Async runs** return `{run_id, status: "running"}`: poll `agent_status(run_id)`, then
   `agent_result(run_id)`. Poll sparingly (every 20 to 30 s at most); do other useful work in between.
   `cancel_agent(run_id)` stops a run you no longer need.
6. **Judge the result, do not trust it.** The summary is another model's claim. Read the code it
   changed with `get_diff(run_id)` (use `stat: true` first for big changes) and run the tests yourself
   before reporting success to the user.
7. **`apply_changes(run_id)`** only for worktree runs you have reviewed. It never applies silently: it
   returns the plan, and the user confirms in the Selachi window or with
   `selachi-agent apply <run_id>` in a terminal. Tell the user what is waiting for their confirmation and
   what it contains. Do not call it in a loop or try to work around the confirmation.

## Profiles

- `run_agent({profile: "Tests", prompt, cwd})`: the agent comes from the profile; pass `agent` only to
  override it. Leave `permission` out to get the profile's own permission; if you do pass it, it can only
  **narrow** the profile's, never widen it.
- Each profile in `list_profiles` has `permission` (what it asks for) and **`effective_permission`**
  (what it will really get for you: the minimum of the profile, the delegation settings and your own
  permission). Trust `effective_permission`; a lowered permission is not an error, the run goes ahead
  with the lower one and says so in `notes`.
- A profile can never raise permission, depth, timeout or budget above the delegation limits. Do not
  try to get around a limit by editing profile files, by asking for another permission, or by running
  `selachi-agent` yourself with different flags. If a profile is too restrictive for the task, say so to
  the user and let them decide.
- Match the profile to the task, not the other way round: a read-only reviewer is for reviewing, not for
  fixing; if the work needs edits, use a profile that allows them (in its worktree) or ask the user.
- `output_format: "json"` profiles reply with a JSON document: parse it, and treat a `notes` entry saying
  the reply is not valid JSON as a failed run.
- Tool restrictions (`allowed_tools` / `denied_tools`) are enforced for Claude only; for other agents they
  are advice in the prompt. Do not rely on them for safety with a non-Claude agent.
- The exact prompt a profile run sent is kept in the run record (visible in Selachi's subagent map); you
  can preview it without running anything with `selachi-agent profiles dry-run <name> "<prompt>"`.
- Never write or change profile files on your own; creating them is the user's decision
  (`selachi-agent profiles init` / `import-claude`).

## Approvals

Whatever the delegated agent wants to run that needs a yes goes through Selachi, not through you:

- `ask` (the user's default): every request goes to the attention queue of the Selachi window and waits
  for the user. A long queue is normal for a worktree run that edits a lot; prefer `read_only` or a profile.
- `allowlist`: commands that match the profile's `allowed_commands` (`cargo *`, `git diff *`, `rg *`) are
  approved on their own; anything else is asked. A command that chains or redirects (`;`, `|`, `&&`, `>`,
  backticks, `$(`) never matches.
- `auto`: nothing is asked.
- The `approvals` you pass can only **lower** the mode the user configured (Settings, Agents, Delegation,
  and the profile's own `approvals`); asking for `auto` when the user chose `ask` changes nothing, and the
  run's `notes` say it was lowered. The effective mode is in the run record (`limits.approvals`).
- In every mode a fixed blocklist is **denied without asking**: `git push`, `git commit`, `git stash`,
  `git clean`, `git reset --hard`, `git checkout --`, `rm -rf` / `Remove-Item -Recurse` outside the project,
  `del /s`, `format`, `diskpart`, `reg add|delete`, and anything that touches credential files (`.env`,
  `.ssh`, `.aws`, `.npmrc`, ...). If the child needs one of those, do it yourself after the run, with the
  user's say-so. Denials show up in the result's `notes`.
- «Permitir siempre en este perfil»: when the user answers an approval with that button (`always_profile`),
  Selachi saves the command's pattern (`cargo test -p x` becomes `cargo test *`) in the profile's
  `allowed_commands` and approves matching commands without asking for the rest of that run and for
  later runs of the profile. It is the user's decision in the Selachi window: you never trigger it, and
  without a profile (or for a command that chains, runs a shell or an interpreter) it is a single yes.
  Do not edit `allowed_commands` yourself.
- Every run also writes a live events log, `agent-runs/<run_id>.events.jsonl` in Selachi's data folder (one
  JSON line per message chunk, tool start / end with its output tail, file edit, approval and status, all
  redacted, capped at 5 MB). It is for the user's activity view; you do not need to read it, use
  `agent_status` / `agent_result`.
- Do not try to talk the child into approving something itself, and do not wrap a blocked command in
  `cmd /c` or a script to get it past the check.

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
  - `unknown_profile`: call `list_profiles` and pick a listed name, or use an explicit `agent`.
  - `invalid_profile`: the profile is broken (a missing or out-of-project context file, a bad schema);
    tell the user, do not work around it.
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
list_profiles({cwd?: string})
run_agent({agent | profile, prompt, cwd, model?, permission?: "read_only"|"ask"|"accept_edits",
           worktree?: bool, timeout_s?: int, budget_tokens?: int,
           approvals?: "ask"|"allowlist"|"auto", async?: bool})
agent_status({run_id})     agent_result({run_id})     cancel_agent({run_id})
get_diff({run_id, stat?: bool})        apply_changes({run_id})   // plan only, user confirms
```

Fallback without MCP: the same engine from any shell, `selachi-agent run codex --cwd . --worktree
--permission read-only --json "<prompt>"` (see `selachi-agent --help`). With a profile:
`selachi-agent run --profile Tests --cwd . --json "<prompt>"`; list them with `selachi-agent profiles list`.
`--approvals ask|allowlist|auto` is the same as the `approvals` argument.
