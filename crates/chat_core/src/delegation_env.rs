//! Environment every agent process of a chat thread gets, so the `selachi-agent` CLI or MCP server
//! that agent starts links its delegations to the thread and is capped by the thread's permission.
//!
//! The values are fixed when the process is spawned. A permission lowered later (the pill) restarts
//! the agent, which refreshes them; the harness also re-reads the thread's current permission from
//! `chat.db` and uses the minimum of both. Names are the harness's (`agent_harness::policy`).

use agents_driver::PermissionMode;

/// Thread id of the calling session.
pub const ENV_PARENT_THREAD: &str = "SELACHI_PARENT_THREAD";
/// Permission of the calling session (`read-only`, `ask`, `accept-edits`).
pub const ENV_PARENT_PERMISSION: &str = "SELACHI_PARENT_PERMISSION";
/// Delegation depth of the calling session (0 = a top-level chat thread).
pub const ENV_DELEGATION_DEPTH: &str = "SELACHI_DELEGATION_DEPTH";
/// Terminal tab of the calling session (never a chat thread's).
pub const ENV_PARENT_TAB: &str = "SELACHI_PARENT_TAB";
/// How the harness was reached (`cli` | `mcp`); a chat's agent decides that itself.
pub const ENV_CHANNEL: &str = "SELACHI_CHANNEL";

/// What a delegation may inherit from a session with `mode`: `full` is never delegable, so it
/// travels as `accept-edits` (the harness still caps it with `[delegation] max_permission`).
pub fn delegable_permission(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read-only",
        PermissionMode::Ask => "ask",
        PermissionMode::AcceptEdits | PermissionMode::Full => "accept-edits",
    }
}

/// `ThreadConfig::env` of a chat thread: sets the three parent variables and removes inherited
/// stale ones (a tab or channel value of the process that launched the app).
pub fn delegation_env(
    thread_id: i64,
    mode: PermissionMode,
    depth: u32,
) -> Vec<(String, Option<String>)> {
    vec![
        (ENV_PARENT_THREAD.into(), Some(thread_id.to_string())),
        (
            ENV_PARENT_PERMISSION.into(),
            Some(delegable_permission(mode).into()),
        ),
        (ENV_DELEGATION_DEPTH.into(), Some(depth.to_string())),
        (ENV_PARENT_TAB.into(), None),
        (ENV_CHANNEL.into(), None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_is_clamped_and_stale_values_are_removed() {
        let env = delegation_env(7, PermissionMode::Full, 0);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get(ENV_PARENT_THREAD), Some(Some("7".into())));
        assert_eq!(
            get(ENV_PARENT_PERMISSION),
            Some(Some("accept-edits".into()))
        );
        assert_eq!(get(ENV_DELEGATION_DEPTH), Some(Some("0".into())));
        assert_eq!(get(ENV_PARENT_TAB), Some(None));
        assert_eq!(get(ENV_CHANNEL), Some(None));
        for (mode, want) in [
            (PermissionMode::ReadOnly, "read-only"),
            (PermissionMode::Ask, "ask"),
            (PermissionMode::AcceptEdits, "accept-edits"),
        ] {
            let e = delegation_env(1, mode, 1);
            assert_eq!(e[1].1.as_deref(), Some(want));
            assert_eq!(e[2].1.as_deref(), Some("1"));
        }
    }
}
