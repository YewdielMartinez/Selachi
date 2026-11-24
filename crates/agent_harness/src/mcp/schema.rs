//! Tool definitions (`tools/list`) and the argument validator that is driven by those same schemas,
//! so what is advertised and what is enforced cannot drift apart.
//!
//! Descriptions are English with a Spanish hint (`ES:`) for the people who read them in a client UI.

use serde_json::{json, Value};

pub const TOOL_NAMES: [&str; 7] = [
    "list_agents",
    "run_agent",
    "agent_status",
    "agent_result",
    "cancel_agent",
    "get_diff",
    "apply_changes",
];

fn run_id_prop() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "Run id returned by run_agent. ES: id de la ejecución devuelto por run_agent."
    })
}

fn tool(name: &str, title: &str, description: &str, schema: Value, annotations: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": schema,
        "annotations": annotations,
    })
}

/// All tools, in a stable order.
pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "list_agents",
            "List coding agents",
            "List the coding agents Selachi can delegate to (claude, codex, opencode, gemini, agy): installed or not, \
             version, whether delegation to each is allowed, current usage limits, and the limits that apply to YOU \
             (maximum permission you may grant, depth, runs in flight, daily token budget). \
             ES: lista los agentes a los que se puede delegar, con sus límites de uso y los límites que te aplican.",
            json!({
                "type": "object",
                "properties": {
                    "models": {
                        "type": "boolean",
                        "description": "Also ask each installed agent for its models (a handshake, no prompt is sent; can take several seconds). Default false. ES: incluir los modelos de cada agente (más lento)."
                    }
                },
                "additionalProperties": false
            }),
            json!({"title": "List coding agents", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}),
        ),
        tool(
            "run_agent",
            "Delegate a task to another agent",
            "Delegate a self-contained task to another coding agent running under Selachi's limits (permission never above \
             yours, isolated worktree on request, timeout, token budget, secrets redacted, approvals go to the Selachi window). \
             Blocks until the agent finishes unless async is true; then use agent_status / agent_result. \
             The prompt must be self-contained and must NOT contain secrets. Returns run_id, status, summary, files_changed, \
             diff_stat, usage, cost and worktree_path. With permission accept_edits use worktree=true and review with \
             get_diff before apply_changes. ES: delega una tarea autocontenida en otro agente; nunca incluyas secretos en el prompt.",
            json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Agent to run: claude | codex | opencode | gemini | agy (antigravity). ES: agente que ejecutará la tarea."
                    },
                    "prompt": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Self-contained task description (no secrets). ES: la tarea, completa y sin secretos."
                    },
                    "cwd": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Absolute path of the project directory the agent works in. ES: ruta absoluta del proyecto."
                    },
                    "model": {
                        "type": "string",
                        "description": "Model for the agent (see list_agents models=true; Claude accepts sonnet | opus | haiku). Optional. ES: modelo opcional."
                    },
                    "permission": {
                        "type": "string",
                        "enum": ["read_only", "ask", "accept_edits"],
                        "description": "read_only (default): the agent only reads. ask: edits need approval in Selachi. accept_edits: edits allowed (use with worktree). Never more than your own permission; full is never delegable. ES: permiso del agente hijo (nunca mayor que el tuyo)."
                    },
                    "worktree": {
                        "type": "boolean",
                        "description": "Run in a new git worktree and branch so your working tree is untouched (needs a git repo). Default false. ES: ejecutar en un worktree aparte."
                    },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 86400,
                        "description": "Seconds before the run is interrupted. Default 900 (15 min). ES: tiempo máximo en segundos."
                    },
                    "budget_tokens": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Interrupt the agent after this many billable tokens (input + output + cache writes). ES: presupuesto de tokens."
                    },
                    "async": {
                        "type": "boolean",
                        "description": "Return immediately with the run_id and poll with agent_status / agent_result. Default false (blocks). ES: devolver al instante y consultar después."
                    }
                },
                "required": ["agent", "prompt", "cwd"],
                "additionalProperties": false
            }),
            json!({"title": "Delegate a task to another agent", "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true}),
        ),
        tool(
            "agent_status",
            "Status of a delegated run",
            "Status of a run: running | done | error | cancelled | timed_out | budget_exceeded, elapsed time, agent, worktree and whether it was applied. ES: estado de una ejecución delegada.",
            json!({"type": "object", "properties": {"run_id": run_id_prop()}, "required": ["run_id"], "additionalProperties": false}),
            json!({"title": "Status of a delegated run", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}),
        ),
        tool(
            "agent_result",
            "Result of a delegated run",
            "Final result of a run (summary, files_changed, diff_stat, usage, cost, worktree_path). If the run is still going, returns status running. ES: resultado final de una ejecución.",
            json!({"type": "object", "properties": {"run_id": run_id_prop()}, "required": ["run_id"], "additionalProperties": false}),
            json!({"title": "Result of a delegated run", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}),
        ),
        tool(
            "cancel_agent",
            "Cancel a delegated run",
            "Stop a running delegation (the agent and everything it started are ended). ES: cancela una ejecución en curso.",
            json!({"type": "object", "properties": {"run_id": run_id_prop()}, "required": ["run_id"], "additionalProperties": false}),
            json!({"title": "Cancel a delegated run", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false}),
        ),
        tool(
            "get_diff",
            "Diff of a delegated run",
            "Unified diff of what the run changed (secrets redacted, capped at 200 000 characters). stat=true returns only the per-file summary. ES: cambios de la ejecución.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": run_id_prop(),
                    "stat": {"type": "boolean", "description": "Only the diff stat, not the patch. Default false. ES: solo el resumen por archivo."}
                },
                "required": ["run_id"],
                "additionalProperties": false
            }),
            json!({"title": "Diff of a delegated run", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}),
        ),
        tool(
            "apply_changes",
            "Bring a worktree run's changes into the project",
            "Prepare to bring the changes of a worktree run into the project. It NEVER applies silently: it returns the dry-run plan \
             (files, diff stat, conflicts, risk). The user confirms in the Selachi window when one is open, or runs \
             `selachi-agent apply <run_id>` in a terminal; if nobody confirms, nothing is applied. Tell the user what is pending. \
             ES: devuelve la vista previa; nunca aplica sin que el usuario confirme en Selachi o en la terminal.",
            json!({"type": "object", "properties": {"run_id": run_id_prop()}, "required": ["run_id"], "additionalProperties": false}),
            json!({"title": "Apply a run's changes (needs user confirmation)", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false}),
        ),
    ]
}

/// The `inputSchema` of `name`.
pub fn input_schema(name: &str) -> Option<Value> {
    tools()
        .into_iter()
        .find(|t| t["name"] == name)
        .map(|t| t["inputSchema"].clone())
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validate `args` against a (flat, object) schema: `required`, `additionalProperties: false`, property
/// `type` / `enum` / `minimum` / `maximum` / `minLength`. Returns the first problem as a sentence.
pub fn validate(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(obj) = args.as_object() else {
        return Err(format!(
            "the arguments must be an object, not {}",
            type_name(args)
        ));
    };
    let empty = serde_json::Map::new();
    let props = schema["properties"].as_object().unwrap_or(&empty);
    if let Some(req) = schema["required"].as_array() {
        for r in req.iter().filter_map(Value::as_str) {
            match obj.get(r) {
                None | Some(Value::Null) => return Err(format!("`{r}` is required")),
                Some(_) => {}
            }
        }
    }
    for (k, v) in obj {
        let Some(p) = props.get(k) else {
            if schema["additionalProperties"] == Value::Bool(false) {
                let known: Vec<&str> = props.keys().map(String::as_str).collect();
                return Err(format!(
                    "unknown argument `{k}` (accepted: {})",
                    known.join(", ")
                ));
            }
            continue;
        };
        if v.is_null()
            && !schema["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|x| x == k.as_str()))
        {
            continue; // an explicit null for an optional argument means "not given"
        }
        if let Some(t) = p["type"].as_str() {
            let ok = match t {
                "integer" => v.is_i64() || v.is_u64(),
                "number" => v.is_number(),
                other => type_name(v) == other,
            };
            if !ok {
                return Err(format!("`{k}` must be of type {t}, not {}", type_name(v)));
            }
        }
        if let Some(e) = p["enum"].as_array() {
            if !e.contains(v) {
                let all: Vec<String> = e.iter().map(|x| x.to_string()).collect();
                return Err(format!("`{k}` must be one of {} (got {v})", all.join(", ")));
            }
        }
        if let (Some(min), Some(n)) = (p["minimum"].as_i64(), v.as_i64()) {
            if n < min {
                return Err(format!("`{k}` must be at least {min}"));
            }
        }
        if let (Some(max), Some(n)) = (p["maximum"].as_i64(), v.as_i64()) {
            if n > max {
                return Err(format!("`{k}` must be at most {max}"));
            }
        }
        if let (Some(min), Some(s)) = (p["minLength"].as_u64(), v.as_str()) {
            if (s.trim().chars().count() as u64) < min {
                return Err(format!("`{k}` must not be empty"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_well_formed_schema() {
        let all = tools();
        assert_eq!(
            all.iter()
                .map(|t| t["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            TOOL_NAMES
        );
        for t in &all {
            let name = t["name"].as_str().unwrap();
            assert!(
                name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{name}"
            );
            assert!(
                t["title"].is_string() && t["description"].as_str().unwrap().contains("ES:"),
                "{name} needs a Spanish hint"
            );
            let s = &t["inputSchema"];
            assert_eq!(s["type"], "object", "{name}");
            assert_eq!(s["additionalProperties"], false, "{name}");
            let props = s["properties"].as_object().unwrap();
            for (k, p) in props {
                assert!(p["type"].is_string(), "{name}.{k} needs a type");
                assert!(
                    p["description"].as_str().is_some_and(|d| d.contains("ES:")),
                    "{name}.{k} needs an EN+ES description"
                );
            }
            for r in s["required"].as_array().into_iter().flatten() {
                assert!(
                    props.contains_key(r.as_str().unwrap()),
                    "{name}: required {r} is not a property"
                );
            }
            assert!(t["annotations"]["readOnlyHint"].is_boolean());
        }
    }

    #[test]
    fn validator_follows_the_schema() {
        let s = input_schema("run_agent").unwrap();
        let ok = json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "permission": "ask", "timeout_s": 5, "async": true});
        assert!(validate(&s, &ok).is_ok());
        let bad = |v: Value| validate(&s, &v).unwrap_err();
        assert!(bad(json!({"prompt": "x", "cwd": "C:/p"})).contains("`agent` is required"));
        assert!(
            bad(json!({"agent": "codex", "prompt": "  ", "cwd": "C:/p"}))
                .contains("`prompt` must not be empty")
        );
        assert!(
            bad(json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "permission": "full"}))
                .contains("one of")
        );
        assert!(
            bad(json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "timeout_s": 0}))
                .contains("at least 1")
        );
        assert!(
            bad(json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "timeout_s": "5"}))
                .contains("must be of type integer")
        );
        assert!(
            bad(json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "worktree": 1}))
                .contains("boolean")
        );
        assert!(
            bad(json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "bogus": 1}))
                .contains("unknown argument `bogus`")
        );
        assert!(bad(json!(["x"])).contains("must be an object"));
        // an explicit null for an optional argument is "not given"
        assert!(validate(
            &s,
            &json!({"agent": "codex", "prompt": "x", "cwd": "C:/p", "model": null})
        )
        .is_ok());
        assert!(validate(&input_schema("list_agents").unwrap(), &json!({})).is_ok());
    }
}
