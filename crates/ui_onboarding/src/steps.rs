//! The six step bodies.

use gpui::{
    AnyElement, Context, Div, ImageSource, IntoElement, ObjectFit, ParentElement, SharedString,
    Styled, Window, div, img, prelude::*, px,
};
use store::config::{PredictionSource, TabMode};
use store::{BuiltinPreset, Density, ProjectKind};
use theme::{Radius, Theme};
use ui_kit::{
    Button, GhostButton, Icon, Segment, SegmentedControl, Slider, Spinner, SwatchRow, Toggle,
    VirtualList, WirePreset, Wireframe, agent_icon, icon,
};

use crate::agents::{AgentRow, AgentStatus, short_version};
use crate::flow::Step;
use crate::model::{ACCENTS, ThemeChoice, WorktreeLocation, accent_index};
use crate::projects::kind_label;
use crate::shells::auto_index;
use crate::view::OnboardingView;
use crate::widgets::{
    badge, checkbox, group, group_label, ok_color, radio, row_shell, row_text, setting_row, star, title_block,
    warn_color,
};

/// Callback adaptor: runs `f` on the view for controls whose callback has no event type.
fn cb<A: 'static>(
    cx: &mut Context<OnboardingView>,
    f: impl Fn(&mut OnboardingView, A, &mut Window, &mut Context<OnboardingView>) + 'static,
) -> impl Fn(A, &mut Window, &mut gpui::App) + 'static {
    let this = cx.entity().downgrade();
    move |a, window, app| {
        let _ = this.update(app, |t, cx| f(t, a, window, cx));
    }
}

impl OnboardingView {
    pub(crate) fn step_body(&mut self, window: &mut Window, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let _ = window;
        match self.flow.step() {
            Step::Welcome => self.welcome(cx, xs),
            Step::Agents => self.agents_step(cx, xs),
            Step::Projects => self.projects_step(cx, xs),
            Step::Appearance => self.appearance_step(cx, xs),
            Step::Terminal => self.terminal_step(cx, xs),
            Step::Done => self.done_step(cx, xs),
        }
    }

    // ---- 1. Bienvenida -----------------------------------------------------------------------

    /// The shark: a frame of the decoded GIF (the first one with reduced motion), on a dark card
    /// in light mode because the shark is white. A same-size empty box while it loads.
    pub(crate) fn hero(&self, t: &Theme) -> AnyElement {
        let (w, h) = (360.0, 202.5);
        let frame = self.shark.as_ref().and_then(|s| s.frames.get(s.ix));
        let picture = match frame {
            Some(f) => img(ImageSource::Render(f.image.clone())).w(px(w)).h(px(h)).object_fit(ObjectFit::Contain).into_any_element(),
            None => div().w(px(w)).h(px(h)).into_any_element(),
        };
        let light = t.mode == theme::Mode::Light;
        div()
            .flex_none()
            .p(px(if light { 14.0 } else { 0.0 }))
            .rounded(px(Radius::LG))
            .when(light, |d| d.bg(Theme::dark().colors.bg))
            .child(picture)
            .into_any_element()
    }

    fn welcome(&self, _cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(20.0))
            .when(!xs, |d| d.child(self.hero(&t)))
            .child(title_block(
                &t,
                xs,
                "Selachi",
                "Una terminal rápida y ligera para trabajar con agentes de IA. Te preparamos todo en un minuto.",
            ))
            .into_any_element()
    }

    // ---- 2. Agentes --------------------------------------------------------------------------

    fn agent_row(&self, ix: usize, row: &AgentRow, cx: &mut Context<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let cli = row.cli;
        let status = row.status;
        let subtitle: Option<SharedString> = match status {
            AgentStatus::NotInstalled => Some(cli.install_command().into()),
            AgentStatus::Checking => None,
            _ => row.version.as_deref().map(|v| format!("v{}", short_version(v)).into()),
        };
        let trailing = match status {
            AgentStatus::Checking => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .text_size(px(12.0))
                .text_color(c.muted)
                .child(Spinner::new(c.accent))
                .child(status.label())
                .into_any_element(),
            AgentStatus::Ready => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(12.0))
                .text_color(c.text)
                .child(icon(Icon::Check, 16.0, ok_color(&t)))
                .child(status.label())
                .into_any_element(),
            AgentStatus::NeedsLogin | AgentStatus::Unverified => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .child(div().text_size(px(12.0)).text_color(if status == AgentStatus::NeedsLogin { warn_color(&t) } else { c.muted }).child(status.label()))
                .child(
                    Button::tonal(format!("login-{ix}"), &t, "Iniciar sesión")
                        .on_click(cx.listener(move |this, _, _, cx| this.open_login(cli, cx))),
                )
                .into_any_element(),
            AgentStatus::NotInstalled => {
                let copied = self.copied == Some(cli);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().text_size(px(12.0)).text_color(c.muted).child(status.label()))
                    .child(
                        Button::tonal(format!("install-{ix}"), &t, if copied { "Copiado" } else { "Copiar comando" })
                            .icon(if copied { Icon::Check } else { Icon::Snippet })
                            .on_click(cx.listener(move |this, _, window, cx| this.copy_install(cli, window, cx))),
                    )
                    .into_any_element()
            }
        };
        row_shell(("agent", ix), &t, false, false)
            .child(agent_icon(cli.mark(), 20.0, if status == AgentStatus::NotInstalled { c.muted } else { c.text }))
            .child(row_text(&t, cli.name(), subtitle, status == AgentStatus::NotInstalled))
            .child(trailing)
            .into_any_element()
    }

    fn agents_step(&self, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let mut list = group(&t);
        for (ix, row) in self.agents.iter().enumerate() {
            list = list.child(self.agent_row(ix, row, cx));
        }
        let hook = Toggle::new("hook", &t)
            .on(self.draft.claude_statusline_hook)
            .on_change(cb(cx, |this, on: bool, _, cx| {
                this.draft.claude_statusline_hook = on;
                cx.notify();
            }));
        let oauth = Toggle::new("oauth", &t).on(self.draft.oauth_endpoint).on_change(cb(cx, |this, on: bool, _, cx| {
            this.draft.oauth_endpoint = on;
            cx.notify();
        }));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(title_block(&t, xs, "Conecta tus agentes", "Selachi usa la sesión de cada CLI: no te pide claves de API ni guarda credenciales."))
            .child(list)
            .child(
                div().w_full().flex().flex_row().justify_end().child(
                    GhostButton::new("recheck", &t, "Volver a comprobar")
                        .on_click(cx.listener(|this, _, window, cx| this.start_detection(window, cx))),
                ),
            )
            .child(group_label(&t, "Opciones de uso"))
            .child(
                group(&t)
                    .child(setting_row(
                        &t,
                        xs,
                        "Hook statusLine de Claude",
                        Some("Lee el contexto y el coste exactos de cada sesión. Solo se instala si lo activas.".into()),
                        hook,
                    ))
                    .child(setting_row(
                        &t,
                        xs,
                        "Endpoint OAuth (experimental)",
                        Some("Consulta tus límites de 5 h y 7 días con el token de Claude. No es oficial y puede dejar de funcionar.".into()),
                        oauth,
                    )),
            )
            .child(div().h(px(0.0)).text_color(c.muted))
            .into_any_element()
    }

    // ---- 3. Espacios y proyectos -------------------------------------------------------------

    fn project_row(&self, ix: usize, entity: &gpui::Entity<Self>) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let Some(item) = self.projects.items().get(ix) else { return div().into_any_element() };
        let p = &item.project;
        let toggle = entity.clone();
        let fav = entity.clone();
        let path: SharedString = store::paths::clean_display(&p.path.to_string_lossy()).into();
        let git_badge: AnyElement = if p.is_git && p.kind != ProjectKind::Git {
            badge(&t, "git").into_any_element()
        } else if !p.is_git {
            div().flex_none().text_size(px(11.0)).text_color(c.outline).child("sin git").into_any_element()
        } else {
            div().into_any_element()
        };
        row_shell(("project", ix), &t, false, true)
            .h(px(40.0))
            .py(px(0.0))
            .child(checkbox(("project-check", ix), &t, item.checked).on_click(move |_, _, cx| {
                toggle.update(cx, |this, cx| {
                    this.projects.toggle_checked(ix);
                    cx.notify();
                });
            }))
            .child(row_text(&t, p.name.clone(), Some(path), false))
            .child(badge(&t, kind_label(p.kind)))
            .child(git_badge)
            .child(star(("project-star", ix), &t, item.favorite).on_click(move |_, _, cx| {
                fav.update(cx, |this, cx| {
                    this.projects.toggle_favorite(ix);
                    cx.notify();
                });
            }))
            .into_any_element()
    }

    fn projects_step(&self, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        // Roots.
        let mut roots = group(&t);
        if self.roots.is_empty() {
            roots = roots.child(
                div().px(px(12.0)).py(px(10.0)).text_size(px(13.0)).text_color(c.muted).child("No encontramos carpetas habituales (Desktop, source\\repos, Projects, dev). Añade la tuya."),
            );
        }
        for (ix, root) in self.roots.iter().enumerate() {
            let display: SharedString = store::paths::clean_display(&root.path.to_string_lossy()).into();
            roots = roots.child(
                row_shell(("root", ix), &t, false, true)
                    .on_click(cx.listener(move |this, _, window, cx| this.toggle_root(ix, window, cx)))
                    .child(checkbox(("root-check", ix), &t, root.checked).on_click(cx.listener(move |this, _, window, cx| this.toggle_root(ix, window, cx))))
                    .child(icon(Icon::Folder, 16.0, c.muted))
                    .child(row_text(&t, display, None, false)),
            );
        }
        roots = roots.child(
            div().px(px(6.0)).py(px(4.0)).flex().flex_row().child(
                Button::tonal("add-root", &t, "Añadir carpeta…")
                    .icon(Icon::Plus)
                    .on_click(cx.listener(|this, _, window, cx| this.add_root(window, cx))),
            ),
        );

        // Projects.
        let n = self.projects.len();
        let checked = self.projects.checked_count();
        let header = div()
            .w_full()
            .px(px(8.0))
            .pt(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .text_color(c.muted)
                    .child(if self.scan_running { format!("Buscando… {n} encontrados") } else { format!("{checked} de {n} proyectos marcados") })
                    .when(self.scan_running, |d| d.child(Spinner::new(c.accent))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .child(GhostButton::new("all", &t, "Todos").on_click(cx.listener(|this, _, _, cx| {
                        this.projects.set_all(true);
                        cx.notify();
                    })))
                    .child(GhostButton::new("none", &t, "Ninguno").on_click(cx.listener(|this, _, _, cx| {
                        this.projects.set_all(false);
                        cx.notify();
                    }))),
            );
        let entity = cx.entity();
        let list_body: AnyElement = if n == 0 {
            let msg = if self.scan_running {
                "Buscando proyectos…"
            } else if self.roots.iter().any(|r| r.checked) {
                "No encontramos proyectos en esas carpetas (buscamos Cargo.toml, package.json, .sln, pyproject.toml y .git)."
            } else {
                "Marca al menos una carpeta para buscar proyectos."
            };
            div().h(px(120.0)).w_full().flex().items_center().justify_center().px(px(24.0)).text_size(px(13.0)).text_color(c.muted).text_center().child(msg).into_any_element()
        } else {
            let height = (n as f32 * 44.0).clamp(88.0, 264.0);
            VirtualList::new("projects", n, 44.0, move |range, _, app| {
                range.map(|ix| entity.read(app).project_row(ix, &entity)).collect()
            })
            .track_scroll(self.list_scroll.clone())
            .h(px(height))
            .w_full()
            .into_any_element()
        };

        // Worktrees.
        let seg = match &self.draft.worktrees {
            WorktreeLocation::Beside => 0,
            WorktreeLocation::Inside => 1,
            WorktreeLocation::Custom(_) => 2,
        };
        let worktrees = SegmentedControl::new("worktrees", &t, ["Junto al repo", ".worktrees", "Otra carpeta"])
            .track(c.surface_2)
            .selected(seg)
            .on_change(cb(cx, |this, ix: usize, window, cx| match ix {
                0 => {
                    this.draft.worktrees = WorktreeLocation::Beside;
                    cx.notify();
                }
                1 => {
                    this.draft.worktrees = WorktreeLocation::Inside;
                    cx.notify();
                }
                _ => this.pick_worktrees_dir(window, cx),
            }));
        let worktree_hint: SharedString = match &self.draft.worktrees {
            WorktreeLocation::Beside => "Nuevos worktrees en <repo>-worktrees, junto al repositorio.".into(),
            WorktreeLocation::Inside => "Nuevos worktrees en <repo>\\.worktrees, dentro del repositorio.".into(),
            WorktreeLocation::Custom(p) => format!("Nuevos worktrees en {}.", p.display()).into(),
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(title_block(&t, xs, "Tus proyectos", "Dinos dónde viven y los encontramos. Marca los que quieras ver en la barra lateral y pon una estrella a los favoritos."))
            .child(group_label(&t, "Carpetas raíz"))
            .child(roots)
            .child(header)
            .child(group(&t).p(px(4.0)).child(list_body))
            .child(group_label(&t, "Worktrees de git"))
            .child(group(&t).child(setting_row(&t, xs, "Ubicación por defecto", Some(worktree_hint), worktrees)))
            .into_any_element()
    }

    // ---- 4. Apariencia -----------------------------------------------------------------------

    fn appearance_step(&self, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let d = &self.draft;

        let theme_ctl = SegmentedControl::new("theme", &t, ThemeChoice::ALL.map(ThemeChoice::label))
            .track(c.surface_2)
            .selected(d.theme.index())
            .on_change(cb(cx, |this, ix: usize, _, cx| {
                if let Some(choice) = ThemeChoice::ALL.get(ix) {
                    this.set_theme_choice(*choice, cx);
                }
            }));
        let swatches = SwatchRow::new("accent", &t, ACCENTS.map(|(_, hex)| {
            let (r, g, b) = store::config::parse_hex_color(hex).unwrap_or((0, 0xAA, 0xFF));
            theme::rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
        }))
        .selected(accent_index(&d.accent))
        .on_select(cb(cx, |this, ix: usize, _, cx| {
            if let Some((_, hex)) = ACCENTS.get(ix) {
                this.draft.accent = (*hex).to_string();
                this.restyle(cx);
            }
        }));

        let mut presets = div().w_full().flex().flex_row().flex_wrap().justify_center().gap(px(8.0)).p(px(8.0));
        for (ix, (preset, wire)) in BuiltinPreset::ALL.into_iter().zip(WirePreset::ALL).enumerate() {
            let selected = d.preset == Some(preset);
            presets = presets.child(
                div()
                    .id(("preset", ix))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft.preset = Some(preset);
                        cx.notify();
                    }))
                    .child(Wireframe::preset(&t, wire).cell(3.0).selected(selected))
                    .child(div().text_size(px(12.0)).text_color(if selected { c.text } else { c.muted }).child(wire.label())),
            );
        }

        let translucency = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(
                Slider::new("translucency", &t)
                    .range(0.0, 100.0)
                    .step(5.0)
                    .default_value(0.0)
                    .value(f32::from(d.translucency))
                    .width(if xs { 140.0 } else { 200.0 })
                    .on_change(cb(cx, |this, v: f32, _, cx| {
                        this.draft.translucency = v.round().clamp(0.0, 100.0) as u8;
                        cx.notify();
                    })),
            )
            .child(div().w(px(40.0)).text_size(px(12.0)).text_color(c.muted).text_right().child(format!("{} %", d.translucency)));
        let font_size = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(
                Slider::new("font-size", &t)
                    .range(10.0, 20.0)
                    .step(1.0)
                    .default_value(13.0)
                    .value(d.mono_size)
                    .width(if xs { 140.0 } else { 200.0 })
                    .on_change(cb(cx, |this, v: f32, _, cx| {
                        this.draft.mono_size = v.round().clamp(10.0, 20.0);
                        cx.notify();
                    })),
            )
            .child(div().w(px(40.0)).text_size(px(12.0)).text_color(c.muted).text_right().child(format!("{} pt", d.mono_size)));
        let density = SegmentedControl::new("density", &t, ["Cómoda", "Compacta"])
            .track(c.surface_2)
            .selected(usize::from(d.density == Density::Compact))
            .on_change(cb(cx, |this, ix: usize, _, cx| {
                this.draft.density = if ix == 1 { Density::Compact } else { Density::Comfortable };
                cx.notify();
            }));

        let sample = div()
            .w_full()
            .px(px(14.0))
            .py(px(10.0))
            .rounded(px(12.0))
            .bg(c.surface_0)
            .font_family(t.typography.mono_family)
            .text_size(px(d.mono_size))
            .line_height(px(d.mono_size * 1.4))
            .text_color(c.text)
            .child("PS C:\\proyectos\\selachi> git status")
            .child(div().text_color(c.muted).child("On branch main, nothing to commit"));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(title_block(&t, xs, "Hazlo tuyo", "Elige cómo se ve Selachi. El cambio de tema se aplica ahora mismo a esta pantalla."))
            .child(
                group(&t)
                    .child(setting_row(&t, xs, "Tema", None, theme_ctl))
                    .child(setting_row(&t, xs, "Acento", None, swatches)),
            )
            .child(group_label(&t, "Layout inicial"))
            .child(group(&t).child(presets))
            .child(
                group(&t)
                    .child(setting_row(&t, xs, "Translucidez de la barra lateral", Some("0 % es opaca. Usa el desenfoque de Windows.".into()), translucency))
                    .child(setting_row(&t, xs, "Tamaño de letra de la terminal", None, font_size))
                    .child(div().px(px(12.0)).pb(px(8.0)).child(sample))
                    .child(setting_row(&t, xs, "Densidad", Some("Compacta ajusta los márgenes de listas y barras.".into()), density)),
            )
            .into_any_element()
    }

    // ---- 5. Terminal -------------------------------------------------------------------------

    fn shell_rows(&self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let c = t.colors;
        let mut list = group(&t);
        let Some(shells) = &self.shells else {
            return list.child(
                div().h(px(56.0)).flex().flex_row().items_center().justify_center().gap(px(10.0)).text_size(px(13.0)).text_color(c.muted).child(Spinner::new(c.accent)).child("Buscando shells…"),
            );
        };
        if shells.is_empty() {
            return list.child(div().px(px(12.0)).py(px(12.0)).text_size(px(13.0)).text_color(c.muted).child("No encontramos shells. Selachi usará el de Windows."));
        }
        let auto = auto_index(shells);
        for (ix, shell) in shells.iter().enumerate() {
            let selected = match &self.draft.shell {
                Some(s) => s.name == shell.name,
                None => auto == Some(ix),
            };
            let pick = shell.clone();
            let is_auto = auto == Some(ix);
            list = list.child(
                row_shell(("shell", ix), &t, selected, true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft.shell = if is_auto { None } else { Some(pick.clone()) };
                        cx.notify();
                    }))
                    .child(radio(&t, selected))
                    .child(icon(Icon::Terminal, 16.0, c.muted))
                    .child(row_text(&t, shell.name.clone(), Some(shell.detail.clone().into()), false))
                    .when(is_auto, |d| d.child(badge(&t, "Recomendado"))),
            );
        }
        list
    }

    fn terminal_step(&self, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let d = &self.draft;
        let tabs = [TabMode::Smart, TabMode::Complete, TabMode::Menu, TabMode::Native];
        let tab_ctl = SegmentedControl::new(
            "tab-mode",
            &t,
            [Segment::new("Inteligente"), Segment::new("Recorrer"), Segment::new("Menú"), Segment::new("Nativo")],
        )
        .track(c.surface_2)
        .selected(tabs.iter().position(|m| *m == d.tab).unwrap_or(0))
        .on_change(cb(cx, move |this, ix: usize, _, cx| {
            if let Some(m) = tabs.get(ix) {
                this.draft.tab = *m;
                cx.notify();
            }
        }));
        let tab_hint = match d.tab {
            TabMode::Smart => "Recomendado: acepta la sugerencia gris y, si no hay, abre el menú de opciones.",
            TabMode::Complete => "Recorre las opciones una a una con cada Tab.",
            TabMode::Menu => "Siempre abre el menú de opciones.",
            TabMode::Native => "No toca la tecla Tab de tu shell.",
        };
        let preds = [PredictionSource::History, PredictionSource::HistoryAndPlugin, PredictionSource::Off];
        let pred_ctl = SegmentedControl::new("prediction", &t, ["Historial", "Con plugins", "Desactivada"])
            .track(c.surface_2)
            .selected(preds.iter().position(|p| *p == d.prediction).unwrap_or(0))
            .on_change(cb(cx, move |this, ix: usize, _, cx| {
                if let Some(p) = preds.get(ix) {
                    this.draft.prediction = *p;
                    cx.notify();
                }
            }));
        let restore = Toggle::new("restore", &t).on(d.restore_session).on_change(cb(cx, |this, on: bool, _, cx| {
            this.draft.restore_session = on;
            cx.notify();
        }));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(title_block(&t, xs, "Tu terminal", "Elige el shell con el que abre cada pestaña y cómo te ayuda a escribir."))
            .child(group_label(&t, "Shell predeterminado"))
            .child(self.shell_rows(cx))
            .child(group_label(&t, "Autocompletado"))
            .child(
                group(&t)
                    .child(setting_row(&t, xs, "Tecla Tab", Some(tab_hint.into()), tab_ctl))
                    .child(setting_row(&t, xs, "Predicción", Some("Sugerencias en gris mientras escribes.".into()), pred_ctl)),
            )
            .child(group_label(&t, "Sesión"))
            .child(group(&t).child(setting_row(
                &t,
                xs,
                "Restaurar la sesión al abrir",
                Some("Reabre pestañas, paneles y carpetas. Nunca guarda lo que imprimió la terminal.".into()),
                restore,
            )))
            .into_any_element()
    }

    // ---- 6. Listo ----------------------------------------------------------------------------

    fn done_step(&self, cx: &mut Context<Self>, xs: bool) -> AnyElement {
        let t = self.theme;
        let c = t.colors;
        let mut list = group(&t);
        for (ix, line) in self.summary().into_iter().enumerate() {
            let step = line.step;
            list = list.child(
                row_shell(("summary", ix), &t, false, true)
                    .items_start()
                    .on_click(cx.listener(move |this, _, window, cx| this.go_to(step, window, cx)))
                    .child(div().w(px(if xs { 78.0 } else { 96.0 })).flex_none().text_size(px(12.0)).text_color(c.muted).child(line.title))
                    .child(div().flex_1().min_w_0().text_size(px(13.0)).line_height(px(19.0)).text_color(if line.skipped { c.muted } else { c.text }).child(line.value)),
            );
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(title_block(&t, xs, "Todo listo", "Puedes cambiar cualquiera de estas opciones más tarde en Ajustes, con «Volver a configurar»."))
            .child(list)
            .when_some(self.last_error.clone(), |d, e| d.child(div().w_full().text_size(px(12.0)).text_color(warn_color(&t)).child(e)))
            .into_any_element()
    }
}
