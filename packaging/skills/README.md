# Selachi delegation: MCP server + skill

Lets a coding agent (Claude Code, Antigravity, anything that speaks MCP) hand a self-contained subtask to
another agent (Codex, Gemini, OpenCode, Claude, Antigravity) through Selachi, with limits: the caller's
permission is the ceiling, optional git worktree, timeout, token budget, secret redaction, and no change
reaches your project without your confirmation.

(ES: permite que un agente delegue subtareas en otros a través de Selachi, con límites y sin aplicar
cambios sin tu confirmación.)

## What is here

| Path | What |
|---|---|
| `selachi-delegation/SKILL.md` | The skill: when to delegate, how to call the tools, what never to delegate |
| `.claude-plugin/plugin.json` | Makes this folder a Claude Code plugin (also installable in Antigravity, see below) |

The MCP server itself is `selachi-agent mcp` (crate `agent_harness`): stdio only, no network, no ports.

## 1. Register the MCP server

`selachi-agent.exe` is the console binary of the harness. Use its full path if it is not on your PATH.

```powershell
# Claude Code (user scope = every project; drop -s user for this project only)
claude mcp add -s user selachi -- "C:\Program Files\Selachi\selachi-agent.exe" mcp

# Antigravity (flags go BEFORE the name; -- is only needed when the command starts with a dash)
agy mcp add selachi "C:\Program Files\Selachi\selachi-agent.exe" mcp
```

Check it: `claude mcp list` / `agy mcp list` should show `selachi`. Remove with `claude mcp remove selachi`
/ `agy mcp remove selachi`. Selachi's Settings, Agents, Delegation has buttons that run these commands for
you after asking.

To tighten the ceiling for a particular registration, set the caller's permission in the server's
environment (default and safest: `read-only`):

```powershell
claude mcp add -s user selachi -e SELACHI_PARENT_PERMISSION=ask -- "...\selachi-agent.exe" mcp
```

`SELACHI_PARENT_PERMISSION` (`read-only | ask | accept-edits`), `SELACHI_DELEGATION_DEPTH` and
`SELACHI_PARENT_THREAD` of the server's environment apply to every call it serves. Terminals opened by
Selachi set them for the agents running inside them.

## 2. Install the skill

Claude Code, one of:

```powershell
# a) personal skill
Copy-Item -Recurse selachi-delegation "$env:USERPROFILE\.claude\skills\selachi-delegation"

# b) as a plugin from this folder (local marketplace or --plugin-dir for a session)
claude --plugin-dir "<path to packaging\skills>"
```

Antigravity: this folder is a Claude Code plugin, which `agy plugin install` converts
(`agy plugin validate <dir>` reports "This looks like a Claude Code plugin: `plugin install` converts it").
Install and `agy plugin list` to confirm. Not tested automatically.

## 3. Use it

Ask the agent in plain words ("get a second opinion from Codex on this migration", "have Gemini review my
diff"). The agent calls `list_agents`, then `run_agent`. For edits it uses `permission: accept_edits` with
`worktree: true`; to bring the changes in it calls `apply_changes`, which only returns a plan: you confirm in
the Selachi window or run

```powershell
selachi-agent apply <run_id>          # shows the plan and asks; --dry-run only prints it
```

## Tools

| Tool | Does |
|---|---|
| `list_agents` | Installed agents, limits, and the limits that apply to the caller |
| `list_profiles` | Subagent profiles (named specializations) the caller can pass as `profile` |
| `run_agent` | Delegate a task (blocking, or `async: true`), with an `agent` or a `profile` |
| `agent_status`, `agent_result`, `cancel_agent` | Follow, collect or stop a run |
| `get_diff` | What a run changed (redacted) |
| `apply_changes` | The apply plan; never applies without the user |

## Notes

- Protocol: MCP `2026-07-28` (stateless, `server/discover`) and the handshake revisions
  `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`.
- Blocking `run_agent` calls last as long as the agent works (15 min by default). If your client has a short
  tool timeout, raise it (Claude Code: `MCP_TOOL_TIMEOUT`) or use `async: true`.
- Closing the client cancels its blocking runs; `async` runs keep going and stay readable with
  `selachi-agent status|result <run_id>`.
- Never put secrets in prompts. Prompts and results pass through Selachi's redaction, but that is a net,
  not a licence.
