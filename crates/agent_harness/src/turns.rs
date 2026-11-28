//! What to do when an agent's turn ends.
//!
//! An agent ending its turn does not mean the task is done. Real runs of Antigravity (`agy`) showed
//! two ways a turn ends early, both reported as `done` before this module existed:
//!
//! * **Backgrounded command**: it started `npm run lint; npm run test; npm run build`, did not wait
//!   for it and answered «Ejecutando lint, test y build…; esperando a que finalice en segundo plano»
//!   (runs 15 and 17). Every tool call of such a run has its end event, so the only evidence is the
//!   final text (and, for agents that report it, a command still running when the turn ended).
//! * **Denied command, then silence**: Selachi's rejection of one command (a `.env.local` read, a
//!   `git checkout --`) makes `agy` abort the whole batch of parallel tool calls (the reads and edits
//!   next to it fail in 0 ms) and end its turn within a second, with no answer or a half sentence
//!   (runs 10, 16, 17, 26). ACP carries no reason in a rejection, so the agent never learns why.
//!
//! [`Planner`] decides, after each turn, between finishing and **one automatic follow-up** per
//! cause (so at most [`MAX_FOLLOW_UPS`] extra turns), written in Spanish like the rest of the
//! harness's messages to the person; [`final_problem`] says when the last turn is still unfinished
//! so the run ends as an error («Incompleto»), never as `done`.

/// Follow-ups a run may make in all (one per cause).
pub const MAX_FOLLOW_UPS: usize = 2;

/// Answers longer than this are a report, not a progress line, whatever their last sentence says.
const REPORT_CHARS: usize = 600;

/// The follow-up for a turn that ended with a command still running.
pub const BACKGROUND_FOLLOW_UP: &str = "Espera a que termine el comando y responde con el resultado final. \
No lances comandos en segundo plano: si hace falta, vuelve a ejecutarlo en primer plano y espera su salida. \
Tu respuesta final debe listar los archivos que cambiaste y el resultado (código de salida) de cada comprobación que ejecutaste.";

/// Phrases (lowercase) that say the agent is waiting for something instead of reporting a result.
const WAITING: &[&str] = &[
    "esperando a que",
    "esperando el resultado",
    "esperando la salida",
    "esperando que",
    "espero a que",
    "a la espera de",
    "en segundo plano",
    "en background",
    "sigue ejecutándose",
    "sigue en ejecución",
    "todavía se está ejecutando",
    "aún se está ejecutando",
    "waiting for",
    "waiting on",
    "awaiting",
    "in the background",
    "background command",
    "still running",
    "still in progress",
    "i'll wait",
    "i will wait",
];

/// The last paragraph (at most ~400 characters) of an answer, lowercase.
fn tail_paragraph(answer: &str) -> String {
    let t = answer.trim();
    let last = t
        .rsplit("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty())
        .unwrap_or(t);
    let n = last.chars().count();
    let from = n.saturating_sub(400);
    last.chars().skip(from).collect::<String>().to_lowercase()
}

/// `true` when the answer reads as a progress line of an agent that is waiting for a command rather
/// than as a final report: its last paragraph says it is waiting / running in the background.
pub fn looks_unfinished(answer: &str) -> bool {
    let tail = tail_paragraph(answer);
    !tail.is_empty() && WAITING.iter().any(|w| tail.contains(w))
}

/// A refusal the agent got during a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// The command (or `tool: detail`).
    pub command: String,
    /// Why (the blocklist's sentence, or «el usuario lo rechazó en Selachi»).
    pub reason: String,
}

/// What was seen when a turn ended.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    /// The turn's answer text (empty when it said nothing).
    pub answer: String,
    /// Commands still running when the turn ended (the agent never saw their result).
    pub unfinished_commands: usize,
    /// Requests refused during the turn (the blocklist, the user, the driver's own rules).
    pub denials: Vec<Denial>,
    /// Tool calls that failed without output right after a refusal: the rest of the batch the agent
    /// aborted (reads and edits that never ran).
    pub aborted_calls: usize,
}

/// Why the run asks the agent for another turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowKind {
    Background,
    Denial,
}

impl FollowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FollowKind::Background => "background",
            FollowKind::Denial => "denial",
        }
    }
}

/// The decision after a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Finish,
    FollowUp { kind: FollowKind, text: String },
}

/// Decides between finishing and a follow-up; remembers which causes it already followed up.
#[derive(Debug, Clone)]
pub struct Planner {
    pub wait_for_background: bool,
    background_sent: bool,
    denial_sent: bool,
}

impl Planner {
    pub fn new(wait_for_background: bool) -> Planner {
        Planner {
            wait_for_background,
            background_sent: false,
            denial_sent: false,
        }
    }

    /// Follow-ups made so far.
    pub fn sent(&self) -> usize {
        usize::from(self.background_sent) + usize::from(self.denial_sent)
    }

    /// After a turn that ended normally.
    pub fn after_turn(&mut self, obs: &Observation) -> Next {
        let said_nothing = obs.answer.trim().is_empty();
        let waiting = looks_unfinished(&obs.answer);
        if !obs.denials.is_empty()
            && !self.denial_sent
            && (said_nothing || waiting || obs.aborted_calls > 0)
        {
            self.denial_sent = true;
            return Next::FollowUp {
                kind: FollowKind::Denial,
                text: denial_follow_up(&obs.denials, obs.aborted_calls),
            };
        }
        if self.wait_for_background
            && !self.background_sent
            && (waiting || obs.unfinished_commands > 0)
        {
            self.background_sent = true;
            return Next::FollowUp {
                kind: FollowKind::Background,
                text: BACKGROUND_FOLLOW_UP.to_string(),
            };
        }
        Next::Finish
    }
}

/// After the last turn: why the run cannot be called done, in Spanish; `None` when it can.
pub fn final_problem(wait_for_background: bool, obs: &Observation) -> Option<String> {
    if !wait_for_background {
        return None;
    }
    let short = obs.answer.chars().count() < REPORT_CHARS;
    if obs.unfinished_commands > 0 {
        return Some(format!(
            "Incompleto: el agente terminó el turno con {} comando(s) todavía en ejecución, así que no se conoce su resultado. \
             Relanza la tarea pidiendo que ejecute las comprobaciones en primer plano, o ejecútalas tú.",
            obs.unfinished_commands
        ));
    }
    if short && looks_unfinished(&obs.answer) {
        return Some(
            "Incompleto: el agente terminó diciendo que está esperando a que acabe un comando (en segundo plano) y, tras pedirle que esperara y respondiera, \
             sigue sin dar el resultado final. Los cambios del worktree pueden estar a medias: revisa `get_diff` antes de aplicar y ejecuta tú lint, pruebas y build."
                .to_string(),
        );
    }
    None
}

/// The piece of a chained command (`a; b && c | d`) the refusal is about, when it can be told.
pub fn blocked_piece(command: &str, reason: &str) -> Option<String> {
    let pieces = split_chain(command);
    if pieces.len() < 2 {
        return None;
    }
    // the reason names the offender in backticks: «`git checkout --` discards changes»
    let needle = reason
        .split('`')
        .nth(1)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())?;
    pieces
        .into_iter()
        .find(|p| p.to_lowercase().contains(&needle))
}

fn split_chain(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        let sep = match c {
            ';' | '\n' => true,
            '&' | '|' => {
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                true
            }
            _ => false,
        };
        if sep {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// The follow-up for a turn in which the agent's requests were refused.
pub fn denial_follow_up(denials: &[Denial], aborted_calls: usize) -> String {
    let mut s = String::new();
    for d in denials.iter().take(3) {
        match blocked_piece(&d.command, &d.reason) {
            Some(piece) => s.push_str(&format!(
                "Selachi rechazó el comando `{}` porque {}. La parte rechazada es `{}`; como se rechazó el comando completo, ninguna de sus partes se ejecutó. ",
                one_line(&d.command, 160),
                one_line(&d.reason, 200),
                one_line(&piece, 80)
            )),
            None => s.push_str(&format!(
                "Selachi rechazó `{}` porque {}. No se ejecutó. ",
                one_line(&d.command, 160),
                one_line(&d.reason, 200)
            )),
        }
    }
    if denials.len() > 3 {
        s.push_str(&format!("(y {} rechazos más.) ", denials.len() - 3));
    }
    if aborted_calls > 0 {
        s.push_str(&format!(
            "Al rechazarlo se cancelaron también {aborted_calls} llamada(s) de herramienta del mismo lote (lecturas o ediciones): no se ejecutaron, repítelas por separado si las necesitas. "
        ));
    }
    s.push_str(
        "No intentes rodear el rechazo con otro comando equivalente, ni revertir archivos con git (si `npm install` cambió un lockfile, solo repórtalo). \
         Continúa sin ese comando y termina con un informe final que diga qué archivos cambiaste, el resultado de lo que sí ejecutaste y qué quedó sin hacer por el rechazo.",
    );
    s
}

/// Rules added to the prompt for agents that need them, so their first turn does not end early.
/// `agy` runs with approvals that deny whole batches of tool calls, ends its turn when a command
/// is still running and does not report what it did: the rules address exactly that.
pub fn agent_preamble(agent: &str) -> Option<&'static str> {
    match agent {
        "agy" => Some(AGY_PREAMBLE),
        _ => None,
    }
}

/// The block added for `agy` (see [`agent_preamble`]).
pub const AGY_PREAMBLE: &str = "=== SELACHI: REGLAS PARA ESTE AGENTE ===
- Ejecuta cada comando en primer plano y espera a que termine antes de seguir: nada de `&`, `Start-Process`, `Start-Job`, `nohup` ni comandos «en segundo plano».
- Lint, pruebas y build se ejecutan en primer plano, uno por uno, leyendo su salida completa. No termines tu turno mientras un comando siga en marcha.
- Un comando rechazado no se reintenta ni se rodea con otro equivalente; continúa sin él y dilo en tu informe.
- No leas archivos de credenciales (`.env`, `.env.local`, claves, tokens): se rechazan siempre. Si necesitas un valor, pídelo en tu respuesta.
- Nunca reviertas archivos con git (`git checkout --`, `git restore`, `git reset`, `git stash`). Si `npm install` cambió un lockfile, repórtalo en tu respuesta sin revertirlo.
- Tu respuesta final es un informe: (1) los archivos que cambiaste, (2) el resultado (código de salida) de cada comprobación que ejecutaste, (3) lo que quedó sin hacer y por qué.
=== FIN DE LAS REGLAS ===";

/// `true` for a tool call that failed without telling anything: no output, or output that is just its
/// own title (`File /C:/x/y.rs`), which is how `agy` reports the calls of a batch it aborted.
fn looks_aborted(run: &chat_core::ToolRun) -> bool {
    use chat_core::ToolStatus;
    if !matches!(run.status, ToolStatus::Failed | ToolStatus::Cancelled) {
        return false;
    }
    let out = run.summary.as_deref().unwrap_or("").trim();
    let title = run.title.trim();
    out.is_empty() || out == title || title.starts_with(out) || out.starts_with(title)
}

/// Tool calls in a row, at the end of the last assistant message, that failed without saying why.
/// A long run of them means the agent keeps trying something that cannot work (every read answers
/// with its own path): [`MAX_SILENT_FAILURES`] stops the run with a clear reason.
pub fn trailing_silent_failures(state: &chat_core::ThreadState) -> usize {
    let Some(m) = state.last_assistant() else {
        return 0;
    };
    let runs: Vec<&chat_core::ToolRun> = m.tool_runs().collect();
    runs.iter().rev().take_while(|r| looks_aborted(r)).count()
}

/// Consecutive silent tool failures after which the run is stopped.
pub const MAX_SILENT_FAILURES: usize = 15;

/// What the turn that started at message index `from` left in `state`. `denials` are the refusals the
/// harness collected during that turn.
pub fn observe(state: &chat_core::ThreadState, from: usize, denials: &[Denial]) -> Observation {
    use chat_core::{Role, ToolClass, ToolStatus};
    let msgs: Vec<&chat_core::Message> = state
        .messages
        .iter()
        .skip(from)
        .filter(|m| m.role == Role::Assistant)
        .collect();
    let answer = msgs.last().map(|m| m.plain_text()).unwrap_or_default();
    let mut unfinished_commands = 0;
    let mut aborted_calls = 0;
    for run in msgs.iter().flat_map(|m| m.tool_runs()) {
        if run.class == ToolClass::Command
            && matches!(run.status, ToolStatus::Running | ToolStatus::Cancelled)
        {
            unfinished_commands += 1;
        } else if !denials.is_empty() && run.class != ToolClass::Command && looks_aborted(run) {
            aborted_calls += 1;
        }
    }
    Observation {
        answer,
        unfinished_commands,
        denials: denials.to_vec(),
        aborted_calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final texts of real runs (agent-runs 15 and 17: the whole answer was this one line).
    const RUN_15: &str = "Ejecutando `lint`, `test` y `build` en `@violet-code/admin` (`oxlint` y las 16 suites / 69 pruebas ya pasaron; esperando a que termine `tsc -b && vite build`).";
    const RUN_17: &str =
        "Ejecutando verificación (`lint`, `test` y `build`); esperando a que finalice el comando en segundo plano.";
    /// Real final answers that were complete (runs 21, 24, 25, 27).
    const FINAL_OK: &[&str] = &[
        "- **Componente:** `Dialog` con `placement=\"side\"` y `variant=\"morph\"` (`createDialog`)\n  - **Qué ocurrió:** `startSession()` solo bloquea `document.documentElement.style.overflow = \"hidden\"` cuando `variant` es `'drawer'` o `'sheet'`.\n  - **Workaround:** Ninguno necesario; se eliminó el bloqueo manual de `documentElement.style.overflow`.",
        "- `npm run lint -w @violet-code/admin`: **exit 0** (`oxlint` sin errores).\n- `npm run test -w @violet-code/admin`: **exit 0** (`Test Files 35 passed (35)`).\n- `npm run build -w @violet-code/admin`: **exit 0** (`tsc -b && vite build`, built in `9.07s`).\n\n`HoldConfirmProps` no expone `instanceRef`; se mitigó usando `resetAfter={1600}` para que el control se restablezca automáticamente si la RPC devuelve error.",
    ];

    fn obs(answer: &str) -> Observation {
        Observation {
            answer: answer.to_string(),
            ..Observation::default()
        }
    }

    #[test]
    fn real_progress_lines_look_unfinished_and_real_reports_do_not() {
        assert!(looks_unfinished(RUN_15));
        assert!(looks_unfinished(RUN_17));
        assert!(looks_unfinished(
            "Running the build now, waiting for it to finish."
        ));
        for ok in FINAL_OK {
            assert!(!looks_unfinished(ok), "{ok}");
        }
        assert!(!looks_unfinished(""));
        // only the last paragraph counts: a report that mentions waiting earlier is a report
        assert!(!looks_unfinished(
            "Antes estuve esperando a que terminara el build.\n\nTodo pasó: exit 0 en lint, test y build."
        ));
    }

    #[test]
    fn a_background_turn_gets_one_follow_up_then_is_incomplete() {
        let mut p = Planner::new(true);
        match p.after_turn(&obs(RUN_15)) {
            Next::FollowUp { kind, text } => {
                assert_eq!(kind, FollowKind::Background);
                assert!(text.starts_with(
                    "Espera a que termine el comando y responde con el resultado final"
                ));
            }
            other => panic!("{other:?}"),
        }
        // asked once: the same kind of turn is not asked again
        assert_eq!(p.after_turn(&obs(RUN_17)), Next::Finish);
        assert!(final_problem(true, &obs(RUN_17))
            .unwrap()
            .starts_with("Incompleto"));
        // a report is fine
        assert_eq!(final_problem(true, &obs(FINAL_OK[1])), None);
        // opting out keeps the old behaviour
        let mut off = Planner::new(false);
        assert_eq!(off.after_turn(&obs(RUN_15)), Next::Finish);
        assert_eq!(final_problem(false, &obs(RUN_15)), None);
    }

    #[test]
    fn a_command_still_running_is_unfinished_whatever_the_text() {
        let mut o = obs("Listo.");
        o.unfinished_commands = 1;
        let mut p = Planner::new(true);
        assert!(matches!(
            p.after_turn(&o),
            Next::FollowUp {
                kind: FollowKind::Background,
                ..
            }
        ));
        assert!(final_problem(true, &o).unwrap().contains("en ejecución"));
    }

    #[test]
    fn a_denied_command_then_silence_gets_the_reason_once() {
        let denial = Denial {
            command: "Test-Path C:\\p\\apps\\admin\\.env.local; if (Test-Path C:\\p\\apps\\admin\\.env.local) { Get-Content C:\\p\\apps\\admin\\.env.local }".into(),
            reason: "es un archivo de credenciales (`.env.local`)".into(),
        };
        let o = Observation {
            answer: String::new(),
            denials: vec![denial.clone()],
            aborted_calls: 2,
            ..Observation::default()
        };
        let mut p = Planner::new(true);
        let Next::FollowUp { kind, text } = p.after_turn(&o) else {
            panic!("no follow-up")
        };
        assert_eq!(kind, FollowKind::Denial);
        assert!(text.contains("rechazó"), "{text}");
        assert!(text.contains("archivo de credenciales"), "{text}");
        assert!(text.contains("2 llamada(s)"), "{text}");
        assert!(text.contains("qué quedó sin hacer"), "{text}");
        // capped: a second denial turn finishes
        assert_eq!(p.after_turn(&o), Next::Finish);
        assert_eq!(p.sent(), 1);
        // a denial followed by a real report needs no follow-up
        let mut p2 = Planner::new(true);
        let mut with_report = obs(FINAL_OK[1]);
        with_report.denials = vec![denial];
        assert_eq!(p2.after_turn(&with_report), Next::Finish);
    }

    #[test]
    fn both_causes_use_at_most_two_follow_ups() {
        let mut p = Planner::new(true);
        let mut o = obs(RUN_15);
        o.denials = vec![Denial {
            command: "git checkout -- package-lock.json; git status --short".into(),
            reason: "`git checkout --` descarta cambios".into(),
        }];
        assert!(matches!(
            p.after_turn(&o),
            Next::FollowUp {
                kind: FollowKind::Denial,
                ..
            }
        ));
        assert!(matches!(
            p.after_turn(&o),
            Next::FollowUp {
                kind: FollowKind::Background,
                ..
            }
        ));
        assert_eq!(p.after_turn(&o), Next::Finish);
        assert_eq!(p.sent(), MAX_FOLLOW_UPS);
    }

    #[test]
    fn a_chained_command_names_the_blocked_piece_and_says_the_rest_did_not_run() {
        let cmd = "git checkout -- package-lock.json; git status --short";
        let piece = blocked_piece(cmd, "`git checkout --` descarta cambios").unwrap();
        assert_eq!(piece, "git checkout -- package-lock.json");
        let text = denial_follow_up(
            &[Denial {
                command: cmd.into(),
                reason: "`git checkout --` descarta cambios".into(),
            }],
            0,
        );
        assert!(text.contains("La parte rechazada es `git checkout -- package-lock.json`"));
        assert!(text.contains("ninguna de sus partes se ejecutó"));
        assert!(text.contains("ni revertir archivos con git"));
        // a single command has no pieces to name
        assert_eq!(blocked_piece("git push", "`git push`"), None);
    }

    #[test]
    fn only_agy_gets_a_preamble_and_it_covers_the_failures_seen() {
        assert!(agent_preamble("claude").is_none());
        assert!(agent_preamble("codex").is_none());
        let p = agent_preamble("agy").unwrap();
        for needle in [
            "primer plano",
            "git checkout",
            "lockfile",
            "archivos que cambiaste",
            "código de salida",
        ] {
            assert!(p.contains(needle), "{needle}");
        }
    }
}
