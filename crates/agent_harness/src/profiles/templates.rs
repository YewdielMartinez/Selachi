//! Starter profiles. They are written **only** by the explicit `selachi-agent profiles init`
//! command (never automatically), into `%APPDATA%\Selachi\agents` or, with `--project`, into
//! `<project>\.selachi\agents`.

use super::{emit_toml, slugify, ProfileFile, TimeoutValue};

const HEADER: &str = "Selachi subagent profile (PLAN 21.3). Edit freely; `selachi-agent profiles show <name>` checks it.\n\
A profile can only narrow a delegation: permission, timeout and budget never exceed [delegation] in config.toml\n\
or the permission of the session that delegates.";

/// The starter profiles: "Revisor de seguridad", "Tests", "Docs", "Investigador".
pub fn starters() -> Vec<ProfileFile> {
    vec![
        ProfileFile {
            name: "Revisor de seguridad".into(),
            description: "Revisa el código en busca de vulnerabilidades, sin modificar nada (solo lectura)".into(),
            agent: "claude".into(),
            effort: Some("high".into()),
            permission: Some("read-only".into()),
            timeout: Some(TimeoutValue::Text("20m".into())),
            allowed_tools: vec!["Read".into(), "Grep".into(), "Glob".into()],
            denied_tools: vec!["Edit".into(), "Write".into(), "NotebookEdit".into(), "WebFetch".into()],
            instructions: "Eres un revisor de seguridad. Solo lees y analizas; no modificas archivos ni ejecutas comandos.\n\
\n\
Busca: inyecciones (SQL, comandos, plantillas), manejo de secretos, autenticación y autorización rotas, \
deserialización insegura, rutas de archivo controladas por el usuario, dependencias peligrosas y condiciones de carrera.\n\
\n\
Para cada hallazgo indica: severidad (crítica, alta, media, baja), archivo y línea, por qué es explotable y una \
corrección concreta. Si no hay hallazgos, dilo. No inventes problemas: cita el código que lo demuestra."
                .into(),
            ..ProfileFile::default()
        },
        ProfileFile {
            name: "Tests".into(),
            description: "Escribe y arregla tests en un worktree aparte; no toca el código de producción".into(),
            agent: "claude".into(),
            effort: Some("medium".into()),
            permission: Some("accept-edits".into()),
            worktree: true,
            timeout: Some(TimeoutValue::Text("20m".into())),
            instructions: "Eres un especialista en tests. Escribe o arregla tests y ejecútalos hasta que pasen.\n\
\n\
- Sigue el framework y las convenciones de tests que ya usa el proyecto.\n\
- No cambies el código de producción; si crees que hay un bug, descríbelo en tu respuesta.\n\
- Cubre el caso normal, los bordes y los errores.\n\
- Termina con la lista de tests añadidos o cambiados y el resultado de ejecutarlos."
                .into(),
            ..ProfileFile::default()
        },
        ProfileFile {
            name: "Docs".into(),
            description: "Escribe o actualiza documentación (README, comentarios de API) en un worktree aparte".into(),
            agent: "claude".into(),
            effort: Some("low".into()),
            permission: Some("accept-edits".into()),
            worktree: true,
            timeout: Some(TimeoutValue::Text("15m".into())),
            instructions: "Eres un redactor técnico. Escribe documentación clara y exacta.\n\
\n\
- Lee el código antes de describirlo; no documentes lo que no existe.\n\
- Edita solo archivos de documentación (Markdown, comentarios de documentación).\n\
- Mantén el estilo y el idioma de la documentación existente.\n\
- Termina con la lista de archivos que tocaste."
                .into(),
            ..ProfileFile::default()
        },
        ProfileFile {
            name: "Investigador".into(),
            description: "Investiga una pregunta sobre el código o el entorno y resume lo hallado (solo lectura)".into(),
            agent: "agy".into(),
            permission: Some("read-only".into()),
            timeout: Some(TimeoutValue::Text("20m".into())),
            instructions: "Eres un investigador. Responde la pregunta explorando el proyecto sin modificar nada.\n\
\n\
Entrega: la respuesta en un párrafo, las pruebas (archivos y líneas) y lo que no pudiste confirmar. \
Distingue claramente lo que viste de lo que supones."
                .into(),
            ..ProfileFile::default()
        },
    ]
}

/// File name (`revisor-de-seguridad.toml`) and text of a starter profile.
pub fn render(p: &ProfileFile) -> (String, String) {
    (
        format!("{}.toml", slugify(&p.name)),
        emit_toml(p, Some(HEADER)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Permission;
    use crate::profiles::{Profile, Scope};
    use std::path::Path;

    #[test]
    fn starters_are_valid_and_match_the_plan() {
        let all = starters();
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Revisor de seguridad", "Tests", "Docs", "Investigador"]
        );
        for p in &all {
            let (file, text) = render(p);
            let parsed = Profile::from_toml(&text, Path::new(&file), Scope::Global)
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(parsed.name, p.name);
            assert!(!parsed.instructions.is_empty());
        }
        let by = |n: &str| {
            Profile::from_toml(
                &render(all.iter().find(|p| p.name == n).unwrap()).1,
                Path::new("x.toml"),
                Scope::Global,
            )
            .unwrap()
        };
        let sec = by("Revisor de seguridad");
        assert_eq!(
            (sec.agent.as_str(), sec.permission, sec.worktree),
            ("claude", Some(Permission::ReadOnly), false)
        );
        let tests = by("Tests");
        assert_eq!(
            (tests.permission, tests.worktree),
            (Some(Permission::AcceptEdits), true)
        );
        assert_eq!(by("Investigador").agent, "agy");
        assert_eq!(render(&all[0]).0, "revisor-de-seguridad.toml");
    }
}
