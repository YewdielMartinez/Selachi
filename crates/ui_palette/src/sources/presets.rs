//! Diseño: the layout presets (PLAN section 17): the five built-in ones plus the user's custom
//! presets by name. Picking one emits [`PaletteEvent::ApplyPreset`].

use store::{BuiltinPreset, PresetId};
use ui_kit::Icon;

use super::{ordered_score, top_n};
use crate::item::{ItemAction, PaletteEvent, PaletteItem};
use crate::query::Scope;
use crate::scorer::{Matcher, Needle};
use crate::source::{PaletteSource, SourceQuery};

pub struct PresetsSource {
    custom: Vec<String>,
}

impl PresetsSource {
    /// `custom`: names of `[[layout.custom_presets]]` (an empty list = only the built-in ones).
    pub fn new(custom: Vec<String>) -> Self {
        Self { custom }
    }
}

fn describe(p: BuiltinPreset) -> (&'static str, &'static str) {
    match p {
        BuiltinPreset::Terminal => (
            "Terminal",
            "Rail, pestañas arriba y la terminal; sin sidebar",
        ),
        BuiltinPreset::Espacios => (
            "Espacios",
            "Sidebar con espacios y agentes; pestañas arriba",
        ),
        BuiltinPreset::Orquestador => (
            "Orquestador",
            "Proyecto, worktree y agentes; pestañas verticales",
        ),
        BuiltinPreset::Chat => ("Chat", "Hilos agrupados, conversación y panel de contexto"),
        BuiltinPreset::Zen => ("Zen", "Solo la terminal; el resto al pasar por el borde"),
        BuiltinPreset::EspaciosUnificados => (
            "Espacios unificados",
            "Sidebar de cristal con espacios y pestañas anidadas; paneles a la derecha",
        ),
    }
}

impl PaletteSource for PresetsSource {
    fn id(&self) -> &'static str {
        "presets"
    }

    fn title(&self) -> &'static str {
        "Diseño"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Actions]
    }

    fn order(&self) -> u8 {
        45
    }

    fn empty_limit(&self) -> usize {
        0
    }

    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        let needle = Needle::new(q.text);
        let mut matcher = Matcher::new();
        let mut rows: Vec<(PresetId, String, String)> = BuiltinPreset::ALL
            .into_iter()
            .map(|b| {
                let (name, desc) = describe(b);
                (
                    PresetId::Builtin(b),
                    format!("Diseño: {name}"),
                    desc.to_string(),
                )
            })
            .collect();
        rows.extend(self.custom.iter().map(|n| {
            (
                PresetId::Custom(n.clone()),
                format!("Diseño: {n}"),
                "Diseño propio".to_string(),
            )
        }));
        let mut out = Vec::new();
        for (i, (id, title, desc)) in rows.into_iter().enumerate() {
            let score = if needle.is_empty() {
                Some(ordered_score(i))
            } else {
                matcher
                    .score(
                        &needle,
                        &[(&title, 1.0), ("layout preset diseño", 0.4), (&desc, 0.4)],
                    )
                    .map(|s| s + ordered_score(i) * 0.05)
            };
            let Some(score) = score else { continue };
            out.push(
                PaletteItem::new(
                    format!("preset:{}", id.as_str()),
                    title,
                    ItemAction::emit("Aplicar", PaletteEvent::ApplyPreset(id)),
                )
                .subtitle(desc)
                .icon(Icon::Layout)
                .score(score),
            );
        }
        top_n(out, q.limit)
    }
}
