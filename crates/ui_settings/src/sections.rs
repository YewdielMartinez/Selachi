//! The simple sections: General, Appearance, Git, Processes and About.

use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px};
use store::Density;
use store::config::AutoStop;
use theme::{Rgba, rgb};
use ui_kit::{Button, GhostButton, Slider, SwatchRow};

use crate::keys;
use crate::ui::{self, PillTone, decimal, note, row, status_pill, value_label};
use crate::view::{Section, SettingsEvent, SettingsView};

/// Language ids the UI can be translated to. Only Spanish exists today.
pub const LANGUAGES: &[(&str, &str)] = &[("es", "Español")];

/// Accent choices (the first is Selachi's brand accent, the default).
pub const ACCENTS: [u32; 8] = [
    0x00AAFF, 0x4FC9D6, 0x7FCB8F, 0xE0B96A, 0xFF9AA2, 0xE8707B, 0xBC8CE8, 0xECEEF2,
];

impl SettingsView {
    pub(crate) fn general(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let languages: Vec<(String, String)> = LANGUAGES
            .iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect();
        let lang = self.select(
            cx,
            "general-lang",
            languages,
            &self.cfg.general.language.clone(),
            |s, v, cx| {
                let v = v.to_string();
                s.edit_cfg(cx, |c| c.general.language = v);
            },
        );
        let restore = self.toggle(
            cx,
            "restore-session",
            self.cfg.session.restore,
            |s, on, cx| {
                s.edit_cfg(cx, |c| c.session.restore = on);
            },
        );
        let confirm = self.toggle(
            cx,
            "confirm-close",
            self.cfg.general.confirm_close_running,
            |s, on, cx| {
                s.edit_cfg(cx, |c| c.general.confirm_close_running = on);
            },
        );
        let chord = keys::effective(&self.catalog, &self.cfg.keybindings, "window.fullscreen");
        let f11 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(status_pill(
                &t,
                keys::display_chord(&chord),
                PillTone::Neutral,
            ))
            .child(
                GhostButton::new("fullscreen-change", &t, "Cambiar").on_click(cx.listener(
                    |s, _, _, cx| {
                        s.set_section(Section::Keyboard, cx);
                        s.start_capture("window.fullscreen", cx);
                    },
                )),
            );
        let updates = self.updates_group(narrow, cx);
        vec![
            ui::group(
                &t,
                None,
                vec![
                    row(
                        &t,
                        narrow,
                        "Idioma",
                        Some("Solo español por ahora; el resto de idiomas llegará más adelante.".into()),
                        lang,
                        self.error_for("general.language"),
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Restaurar la sesión al abrir",
                        Some("Reabre las pestañas, los paneles y sus carpetas. Nunca se guarda el contenido de la terminal.".into()),
                        restore,
                        None,
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Confirmar al cerrar con comandos en curso",
                        Some("Avisa antes de cerrar una pestaña o la ventana si algo se sigue ejecutando.".into()),
                        confirm,
                        None,
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Pantalla completa",
                        Some("Oculta la barra del sistema; la de Selachi se mantiene.".into()),
                        f11,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
            updates,
        ]
    }

    /// General > Actualizaciones: look for updates, channel, when to install, state and "Buscar ahora".
    fn updates_group(&mut self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let check = self.toggle(cx, "updates-check", self.cfg.updates.check, |s, on, cx| {
            s.edit_cfg(cx, |c| c.updates.check = on);
        });
        let channel = self.select(
            cx,
            "updates-channel",
            vec![("stable".into(), "Estable".into())],
            "stable",
            |s, v, cx| {
                let v = v.to_string();
                s.edit_cfg(cx, |c| c.updates.channel = v);
            },
        );
        let install = self.segmented(
            cx,
            "updates-install",
            ["Reiniciar y actualizar", "Al cerrar"],
            usize::from(self.cfg.updates.install == "on_exit"),
            |s, ix, cx| {
                let v = if ix == 1 { "on_exit" } else { "now" };
                s.edit_cfg(cx, |c| c.updates.install = v.to_string());
            },
        );
        let status = self.update_status.clone();
        let enabled = self.cfg.updates.check;
        let mut control = div().flex().flex_row().items_center().gap(px(8.0));
        if status.ready {
            control = control.child(
                Button::action("updates-install-now", &t, "Reiniciar y actualizar")
                    .no_icon()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::InstallUpdate))),
            );
        }
        control = control.child(
            Button::tonal(
                "updates-check-now",
                &t,
                if status.busy {
                    "Buscando…"
                } else {
                    "Buscar ahora"
                },
            )
            .disabled(status.busy || !enabled)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CheckUpdates))),
        );
        let text = if status.text.is_empty() {
            format!("Versión {}", self.version)
        } else {
            status.text
        };
        ui::group(
            &t,
            Some("Actualizaciones"),
            vec![
                row(
                    &t,
                    narrow,
                    "Buscar actualizaciones",
                    Some("Mira GitHub Releases (solo HTTPS) a los 30 s de abrir y luego cada 24 h. Apagado: Selachi no toca la red por esto.".into()),
                    check,
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Canal",
                    Some("Solo existe el estable (las releases publicadas, sin borradores ni prereleases).".into()),
                    channel,
                    self.error_for("updates.channel"),
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Instalar",
                    Some("Se descarga y verifica sola (SHA256); nunca se instala sin tu acción. «Al cerrar» lo instala en silencio cuando cierras Selachi.".into()),
                    install,
                    None,
                )
                .into_any_element(),
                row(
                    &t,
                    narrow,
                    "Estado",
                    Some(SharedString::from(text)),
                    control,
                    None,
                )
                .into_any_element(),
            ],
        )
        .into_any_element()
    }

    pub(crate) fn appearance(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let a = self.cfg.appearance.clone();
        let layout = self.cfg.layout.resolve();

        let theme_cards = self.theme_cards(cx);
        let palettes = self.palettes_group(narrow);

        let colors: Vec<Rgba> = ACCENTS.iter().map(|h| rgb(*h)).collect();
        let selected = ACCENTS.iter().position(|h| {
            store::config::parse_hex_color(&a.accent).is_some_and(|(r, g, b)| {
                (u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)) == *h
            })
        });
        let this = cx.entity();
        let swatches = SwatchRow::new("accent", &t, colors)
            .selected(selected)
            .on_select(move |ix, _, cx| {
                this.update(cx, |s, cx| {
                    let hex = format!("#{:06X}", ACCENTS[ix.min(ACCENTS.len() - 1)]);
                    s.errors.remove("accent_hex");
                    s.edit_cfg(cx, |c| c.appearance.accent = hex.clone());
                    // Keep the hex field in step with the swatch.
                    if let Some(f) = s.fields.get("accent_hex") {
                        f.update(cx, |f, cx| f.set_text(&hex, cx));
                    }
                    s.sync_invalid(cx);
                });
            });
        // Swatches get their own line under the label (like the theme cards) so the row can never
        // push them past the card; only the hex field sits in the control slot.
        let accent_control = self.input(cx, "accent_hex", "#00AAFF", 132.0, true);
        let accent_swatches = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .flex_wrap()
            .child(swatches);

        let ui_font = self.input(cx, "ui_font", "DM Sans", 200.0, false);
        let mono_font = self.input(cx, "mono_font", "Geist Mono", 200.0, false);

        let size = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                Slider::new("mono-size", &t)
                    .range(8.0, 24.0)
                    .step(0.5)
                    .default_value(13.0)
                    .value(a.mono_size as f32)
                    .width(if narrow { 200.0 } else { 220.0 })
                    .on_change({
                        let this = cx.entity();
                        move |v, _, cx| {
                            this.update(cx, |s, cx| {
                                s.edit_cfg(cx, |c| c.appearance.mono_size = f64::from(v))
                            });
                        }
                    }),
            )
            .child(value_label(
                &t,
                format!("{} pt", decimal(a.mono_size, 1).trim_end_matches(",0")),
            ));
        let leading = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                Slider::new("line-height", &t)
                    .range(1.0, 2.0)
                    .step(0.05)
                    .default_value(1.3)
                    .value(a.line_height as f32)
                    .width(if narrow { 200.0 } else { 220.0 })
                    .on_change({
                        let this = cx.entity();
                        move |v, _, cx| {
                            let v = (f64::from(v) * 100.0).round() / 100.0;
                            this.update(cx, |s, cx| {
                                s.edit_cfg(cx, |c| c.appearance.line_height = v)
                            });
                        }
                    }),
            )
            .child(value_label(&t, decimal(a.line_height, 2)));

        let density = self.segmented(
            cx,
            "density",
            ["Compacta", "Cómoda"],
            usize::from(layout.density == Density::Comfortable),
            |s, ix, cx| {
                s.edit_cfg(cx, |c| {
                    let mut r = c.layout.resolve();
                    r.density = if ix == 0 {
                        Density::Compact
                    } else {
                        Density::Comfortable
                    };
                    c.layout.set_resolved(&r);
                });
            },
        );
        let glass = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                Slider::new("translucency", &t)
                    .range(0.0, 100.0)
                    .step(5.0)
                    .default_value(0.0)
                    .value(f32::from(layout.sidebar_translucency))
                    .width(if narrow { 200.0 } else { 220.0 })
                    .on_change({
                        let this = cx.entity();
                        move |v, _, cx| {
                            let pct = v.round().clamp(0.0, 100.0) as u8;
                            this.update(cx, |s, cx| {
                                s.edit_cfg(cx, |c| {
                                    let mut r = c.layout.resolve();
                                    r.sidebar_translucency = pct;
                                    c.layout.set_resolved(&r);
                                });
                            });
                        }
                    }),
            )
            .child(value_label(
                &t,
                format!("{} %", layout.sidebar_translucency),
            ));

        let layout_note = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .child(note(
                &t,
                "El preset, las pestañas, la barra lateral, el panel derecho y las divisiones están en Diseño.",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(Button::tonal("open-design", &t, "Más opciones de layout").on_click(cx.listener(
                        |s, _, _, cx| s.set_section(Section::Design, cx),
                    )))
                    .child(GhostButton::new("open-layout-popover", &t, "Panel rápido").on_click(cx.listener(
                        |_, _, _, cx| cx.emit(SettingsEvent::OpenLayoutPanel),
                    ))),
            );

        vec![
            ui::group(
                &t,
                Some("Temas"),
                vec![
                    row(
                        &t,
                        narrow,
                        "Modo",
                        Some("«Sistema» sigue el modo claro u oscuro de Windows. Cada vista previa usa los colores de su tema.".into()),
                        div(),
                        None,
                    )
                    .into_any_element(),
                    ui::plain_row(theme_cards).into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Color de acento",
                        Some("Cursor, foco, progreso, medidores y botones de acción.".into()),
                        accent_control,
                        None, // the field shows its own error
                    )
                    .into_any_element(),
                    ui::plain_row(accent_swatches).into_any_element(),
                ],
            )
            .into_any_element(),
            palettes,
            ui::group(
                &t,
                Some("Tipografía"),
                vec![
                    row(&t, narrow, "Fuente de la interfaz", None, ui_font, None).into_any_element(),
                    row(&t, narrow, "Fuente de la terminal", None, mono_font, None)
                        .into_any_element(),
                    row(&t, narrow, "Tamaño de la terminal", None, size, self.error_for("appearance.mono_size"))
                        .into_any_element(),
                    row(&t, narrow, "Interlineado", None, leading, self.error_for("appearance.line_height"))
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            ui::group(
                &t,
                Some("Superficies"),
                vec![
                    row(&t, narrow, "Densidad", Some("Espaciado de listas y barras.".into()), density, None)
                        .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Translucidez de la barra lateral",
                        Some("Mica/Acrylic de Windows. 0 % = opaca.".into()),
                        glass,
                        None,
                    )
                    .into_any_element(),
                    ui::plain_row(layout_note).into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    pub(crate) fn git(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let confirm = self.toggle(
            cx,
            "git-confirm",
            self.cfg.git.confirm_sensitive,
            |s, on, cx| {
                s.edit_cfg(cx, |c| c.git.confirm_sensitive = on);
            },
        );
        let editor = self.input(cx, "external_editor", "code, cursor, notepad…", 240.0, true);
        let pr = self.toggle(cx, "git-pr-status", self.cfg.git.pr_status, |s, on, cx| {
            s.edit_cfg(cx, |c| c.git.pr_status = on);
        });
        vec![
            ui::group(
                &t,
                None,
                vec![
                    row(
                        &t,
                        narrow,
                        "Confirmar operaciones sensibles",
                        Some("Pregunta antes de un force-push, un reset --hard, borrar una rama y similares.".into()),
                        confirm,
                        None,
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Editor externo",
                        Some("Comando para «Abrir en el editor». Vacío = la aplicación predeterminada de Windows.".into()),
                        editor,
                        None,
                    )
                    .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Estado del PR y de CI",
                        Some("Muestra el pull request de la rama y sus checks (necesita `gh` instalado y con sesión; si no, se oculta). Se consulta al cambiar de rama, tras un push, bajo demanda y al volver a la ventana (máx. cada 5 min): nunca en segundo plano.".into()),
                        pr,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    pub(crate) fn processes(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let ix = match self.cfg.processes.auto_stop_on_thread_close {
            AutoStop::Ask => 0,
            AutoStop::Always => 1,
            AutoStop::Never => 2,
        };
        let control = self.segmented(
            cx,
            "autostop",
            ["Preguntar", "Detener siempre", "No detener"],
            ix,
            |s, ix, cx| {
                let v = [AutoStop::Ask, AutoStop::Always, AutoStop::Never][ix.min(2)];
                s.edit_cfg(cx, |c| c.processes.auto_stop_on_thread_close = v);
            },
        );
        vec![
            ui::group(
                &t,
                None,
                vec![
                    row(
                        &t,
                        narrow,
                        "Al cerrar un hilo con procesos en marcha",
                        Some(
                            "Servidores de desarrollo, watchers y otros procesos que lanzó el agente. «No detener» los deja huérfanos y te avisa."
                                .into(),
                        ),
                        control,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }

    pub(crate) fn about(&mut self, narrow: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = self.theme;
        let c = t.colors;
        let ty = t.typography;
        let dir = self
            .path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let db = store::paths::state_db_path_in(&dir);
        let path_text = |p: &std::path::Path| {
            div()
                .max_w(px(460.0))
                .text_size(px(ty.ui_size_sm))
                .font_family(t.typography.mono_family)
                .text_color(c.muted)
                .child(SharedString::from(p.display().to_string()))
        };
        let open_dir = dir;
        let open = Button::tonal("open-config-dir", &t, "Abrir carpeta de configuración")
            .on_click(cx.listener(move |_, _, _, _| open_folder(&open_dir)));
        let rerun = Button::tonal("rerun-onboarding", &t, "Volver a configurar").on_click(
            cx.listener(|_, _, _, cx| {
                cx.emit(SettingsEvent::RerunOnboarding);
            }),
        );
        vec![
            ui::group(
                &t,
                None,
                vec![
                    row(&t, narrow, "Selachi", Some("Terminal para agentes de IA.".into()), status_pill(&t, format!("v{}", self.version), PillTone::Neutral), None)
                        .into_any_element(),
                    row(&t, narrow, "Configuración", Some("config.toml; se recarga en caliente.".into()), path_text(&self.path), None)
                        .into_any_element(),
                    row(&t, narrow, "Datos", Some("Historial, proyectos y sesión (state.db).".into()), path_text(&db), None)
                        .into_any_element(),
                ],
            )
            .into_any_element(),
            ui::group(
                &t,
                Some("Acciones"),
                vec![
                    row(&t, narrow, "Carpeta de configuración", Some("Se abre en el Explorador de archivos.".into()), open, None)
                        .into_any_element(),
                    row(
                        &t,
                        narrow,
                        "Primer arranque",
                        Some("Repite el asistente: agentes, proyectos, apariencia y terminal. No borra nada.".into()),
                        rerun,
                        None,
                    )
                    .into_any_element(),
                ],
            )
            .into_any_element(),
        ]
    }
}

/// Open a folder in the platform file manager.
fn open_folder(dir: &std::path::Path) {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(windows, target_os = "macos")))]
    let program = "xdg-open";
    if let Err(e) = std::process::Command::new(program).arg(dir).spawn() {
        eprintln!("ui_settings: could not open {}: {e}", dir.display());
    }
}
