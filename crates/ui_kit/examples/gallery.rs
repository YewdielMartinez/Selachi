//! Gallery of every ui_kit component.
//!
//! ```text
//! cargo run -p ui_kit --example gallery --release -- [dark|light|oled] [--closed] [--rings] [--select-open]
//! ```
//!
//! The popover starts open (so screenshots show it); `--closed` starts it closed. Click the
//! "Ajustes" pill to toggle it, Esc or a click outside closes it.
//!
//! Screenshot flags (no input injection needed): `--rings` forces the keyboard focus ring on a
//! sample of controls and focuses the first text field; `--select-open` opens the select.
//! Everything is also reachable with the keyboard: Tab / Shift+Tab, Enter / Space, arrows.

use gpui::{
    App, Application, Bounds, Context, Entity, IntoElement, Render, SharedString, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use theme::{Mode, Theme, rgb};
use ui_kit::{
    ActionSize, Assets, Button, Card, CardTone, Checkbox, DitherMeter, EmptyState, FocusCard,
    GhostButton, Icon, IconButton, ListRow, Logo, Pill, Placement, Popover, RadioGroup, RadioRow,
    Rotation, RowStatus, SectionLabel, SegmentedControl, Select, SelectOption, Slider, Spinner,
    SwatchRow, TextArea, TextField, Toggle, Tooltip, UniformListScrollHandle, VirtualList,
    WirePreset, Wireframe,
};

struct Gallery {
    theme: Theme,
    mode_ix: usize,
    reduce_motion: bool,
    sidebar_alpha: f32,
    accent_ix: usize,
    popover_open: bool,
    preset: usize,
    row_sel: usize,
    active_rail: usize,
    scroll: UniformListScrollHandle,
    name: Entity<TextField>,
    long_path: Entity<TextField>,
    invalid: Entity<TextField>,
    secret: Entity<TextField>,
    frozen: Entity<TextField>,
    notes: Entity<TextArea>,
    select: Entity<Select>,
    select_long: Entity<Select>,
    agreed: bool,
    density: usize,
    radio_row: usize,
    nav_row: usize,
    flagged: Entity<TextField>,
}

impl Gallery {
    fn new(mode: Mode, popover_open: bool, cx: &mut Context<Self>) -> Self {
        let t = Theme::for_mode(mode);
        let name = cx.new(|cx| {
            TextField::new(t, cx)
                .placeholder("Nombre del preset")
                .with_text("Orquestador")
        });
        let long_path = cx.new(|cx| {
            TextField::new(t, cx).monospace(true).with_text(
                "C:\\Users\\venta\\Desktop\\Terminal\\crates\\ui_kit\\src\\text_field\\element.rs",
            )
        });
        let invalid = cx.new(|cx| TextField::new(t, cx).with_text("feat/ui kit"));
        invalid.update(cx, |f, cx| {
            f.set_error(Some("Los nombres de rama no llevan espacios"), cx)
        });
        let secret = cx.new(|cx| TextField::new(t, cx).password(true).with_text("hunter2"));
        let frozen = cx.new(|cx| {
            TextField::new(t, cx)
                .read_only(true)
                .with_text("selachi.toml (solo lectura)")
        });
        let notes = cx.new(|cx| {
            TextArea::area(t, cx)
                .placeholder("Nota para el agente")
                .with_text("Revisar el borrado de presets.\nSi falla, abrir un issue.")
                .lines(2, 5)
        });
        let select = cx.new(|cx| {
            Select::new("gal-select", t, cx)
                .options([
                    SelectOption::new("system", "Sistema"),
                    SelectOption::new("dark", "Oscuro").detail("recomendado"),
                    SelectOption::new("light", "Claro"),
                    SelectOption::new("oled", "OLED").disabled(true),
                ])
                .with_value("dark")
                .width(220.0)
        });
        let select_long = cx.new(|cx| {
            Select::new("gal-select-long", t, cx)
                .options((0..120).map(|i| {
                    SelectOption::new(
                        format!("font-{i}"),
                        format!(
                            "Una familia tipografica con un nombre absurdamente largo numero {i}"
                        ),
                    )
                }))
                .with_value("font-7")
                .max_w(260.0)
        });
        let flagged = cx.new(|cx| TextField::new(t, cx).with_text("sin mensaje, solo borde"));
        flagged.update(cx, |f, cx| f.set_invalid(true, cx));
        Self {
            flagged,
            radio_row: 1,
            nav_row: 1,
            name,
            long_path,
            invalid,
            secret,
            frozen,
            notes,
            select,
            select_long,
            agreed: true,
            density: 1,
            theme: Theme::for_mode(mode),
            mode_ix: match mode {
                Mode::Dark => 1,
                Mode::Light => 2,
                Mode::Oled => 1,
            },
            reduce_motion: false,
            sidebar_alpha: 35.0,
            accent_ix: 0,
            popover_open,
            preset: 2,
            row_sel: 1,
            active_rail: 0,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn row_data(ix: usize) -> (&'static str, &'static str, &'static str, RowStatus) {
        const TITLES: [&str; 6] = [
            "Refactor del parser OSC",
            "Panel de procesos",
            "Migrar status bar a ui_kit",
            "Revisar costes por rama",
            "Persistencia del layout",
            "Visor con tree-sitter",
        ];
        const SUBS: [&str; 6] = [
            "selachi · feat/osc",
            "selachi · proc_svc",
            "selachi · main",
            "selachi · agents_svc",
            "selachi · store",
            "selachi · viewer_svc",
        ];
        const META: [&str; 6] = [
            "opus-5.5 49m",
            "sonnet-5.5 12m",
            "opus-5.5 2h",
            "haiku 3m",
            "sonnet-5.5 1h",
            "opus-5.5 8m",
        ];
        const ST: [RowStatus; 6] = [
            RowStatus::Working,
            RowStatus::Waiting,
            RowStatus::Done,
            RowStatus::Error,
            RowStatus::Done,
            RowStatus::Working,
        ];
        let i = ix % 6;
        (TITLES[i], SUBS[i], META[i], ST[i])
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let this = cx.entity();
        for f in [
            &self.name,
            &self.long_path,
            &self.invalid,
            &self.secret,
            &self.frozen,
            &self.flagged,
        ] {
            f.update(cx, |f, cx| f.set_theme(t, cx));
        }
        self.notes.update(cx, |f, cx| f.set_theme(t, cx));
        for s in [&self.select, &self.select_long] {
            s.update(cx, |s, cx| s.set_theme(t, cx));
        }

        // --- column 1: actions + feedback -------------------------------------------------
        let tabs = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(
                Pill::new("t0", &t)
                    .icon(Icon::Terminal)
                    .label("crates")
                    .active(true),
            )
            .child(
                Pill::new("t1", &t)
                    .icon(Icon::Agents)
                    .label("claude")
                    .on_click(|_, _, _| {}),
            )
            .child(
                Pill::new("t2", &t)
                    .icon(Icon::Files)
                    .label("docs")
                    .on_click(|_, _, _| {}),
            )
            .child(
                IconButton::new("plus", &t, Icon::Plus)
                    .size(28.0)
                    .emphasize_on_hover(true),
            );

        let chips = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Pill::compact("c0", &t).icon(Icon::GitBranch).label("main").tooltip("Rama main · limpia"))
            .child(
                Pill::compact("c1", &t)
                    .label("Claude 12.4M · sem")
                    .child(DitherMeter::new(&t).value(Some(0.42)))
                    .tooltip("Facturables: 12.4M\nEntrada 1.2M · Salida 2.1M · Caché (crear) 9.1M\nLecturas de caché: 84M (no cuentan)\nestimado"),
            )
            .child(Pill::compact("c2", &t).label("RAM 76 MB"));

        let rail = (0..5).fold(div().flex().flex_row().gap(px(4.0)), |row, i| {
            let this = this.clone();
            let icons = [
                Icon::Terminal,
                Icon::Files,
                Icon::GitBranch,
                Icon::Agents,
                Icon::History,
            ];
            row.child(
                IconButton::new(format!("rail{i}"), &t, icons[i])
                    .active(self.active_rail == i)
                    .tooltip("Icon button")
                    .on_click(move |_, _, cx| {
                        this.update(cx, |g, cx| {
                            g.active_rail = i;
                            cx.notify();
                        });
                    }),
            )
        });

        let buttons = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Button::primary("b0", &t, "Primary").on_click(|_, _, _| {}))
            .child(Button::tonal("b1", &t, "Tonal").on_click(|_, _, _| {}))
            .child(Button::ghost("b2", &t, "Ghost").on_click(|_, _, _| {}))
            .child(
                GhostButton::new("b3", &t, "Limpiar")
                    .icon(Icon::Close)
                    .on_click(|_, _, _| {}),
            );
        let buttons2 = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Button::primary("b4", &t, "Nuevo hilo").icon(Icon::Plus))
            .child(Button::primary("b5", &t, "Disabled").disabled(true));

        let tone_chip = |label: &'static str, fill: gpui::Rgba| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(div().size(px(14.0)).rounded_full().bg(fill))
                .child(div().text_size(px(12.0)).text_color(c.muted).child(label))
        };
        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.0))
            .child(Button::action("a0", &t, "Nueva cita").on_click(|_, _, _| {}))
            .child(
                Button::action("a1", &t, "Commit")
                    .no_icon()
                    .size(ActionSize::Lg),
            )
            .child(Button::action("a2", &t, "Deshabilitado").disabled(true));
        let action_tones = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .child(tone_chip("reposo", c.accent_strong))
            .child(tone_chip("hover", c.accent_strong_hover))
            .child(tone_chip("press", c.accent_strong_press))
            .child(tone_chip("señal", c.accent));

        let meters = div().flex().flex_col().gap(px(8.0)).children(
            [None, Some(0.0), Some(0.3), Some(0.55), Some(1.0)]
                .into_iter()
                .map(|v| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.0))
                        .child(
                            div()
                                .w(px(64.0))
                                .text_size(px(12.0))
                                .text_color(c.muted)
                                .child(v.map_or("sin dato".to_string(), |v| {
                                    format!("{:.0} %", v * 100.0)
                                })),
                        )
                        .child(DitherMeter::new(&t).value(v))
                }),
        );

        let status = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(20.0))
            .children(
                [
                    (RowStatus::Working, "working"),
                    (RowStatus::Waiting, "waiting"),
                    (RowStatus::Done, "done"),
                    (RowStatus::Error, "error"),
                ]
                .into_iter()
                .map(|(s, l)| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(ui_kit::StatusGlyph::new(&t, s))
                        .child(div().text_size(px(12.0)).text_color(c.muted).child(l))
                }),
            );

        let col1 = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(360.0))
            .child(SectionLabel::new(&t, "Pills · pestañas"))
            .child(tabs)
            .child(SectionLabel::new(&t, "Pills · barra de estado"))
            .child(chips)
            .child(SectionLabel::new(&t, "IconButton (rail)"))
            .child(rail)
            .child(SectionLabel::new(&t, "Button"))
            .child(buttons)
            .child(buttons2)
            .child(SectionLabel::new(
                &t,
                "Acción (accent_strong · máx. 1 por vista)",
            ))
            .child(actions)
            .child(action_tones)
            .child(SectionLabel::new(&t, "DitherMeter"))
            .child(meters)
            .child(SectionLabel::new(&t, "Estado"))
            .child(status)
            .child(SectionLabel::new(&t, "Spinner"))
            .child(Spinner::new(c.accent));

        // --- column 2: settings + popover + tooltip ---------------------------------------
        let setting = |label: &'static str, control: gpui::AnyElement| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(div().text_size(px(13.0)).text_color(c.text).child(label))
                .child(control)
        };
        let accents = [0x00AAFF, 0x7FCB8F, 0xBC8CE8, 0xE0B96A, 0xE8707B, 0x4FC9D6].map(rgb);
        let settings = Card::new(&t)
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(SectionLabel::new(&t, "Apariencia"))
            .child(setting(
                "Tema",
                SegmentedControl::new("theme", &t, ["Sistema", "Oscuro", "Claro"])
                    .selected(self.mode_ix)
                    .track(c.surface_2)
                    .on_change({
                        let this = this.clone();
                        move |ix, _, cx| {
                            this.update(cx, |g, cx| {
                                g.mode_ix = ix;
                                if ix == 1 {
                                    g.theme = Theme::dark();
                                } else if ix == 2 {
                                    g.theme = Theme::light();
                                }
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element(),
            ))
            .child(setting(
                "Reducir movimiento",
                Toggle::new("reduce", &t)
                    .on(self.reduce_motion)
                    .on_change({
                        let this = this.clone();
                        move |on, _, cx| {
                            ui_kit::set_reduced_motion(on);
                            this.update(cx, |g, cx| {
                                g.reduce_motion = on;
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element(),
            ))
            .child(setting(
                "Breadcrumb en la titlebar",
                Toggle::new("crumb", &t).on(true).into_any_element(),
            ))
            .child(setting(
                "Translucidez de la sidebar",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        Slider::new("alpha", &t)
                            .range(0.0, 100.0)
                            .value(self.sidebar_alpha)
                            .default_value(60.0)
                            .step(1.0)
                            .width(150.0)
                            .on_change({
                                let this = this.clone();
                                move |v, _, cx| {
                                    this.update(cx, |g, cx| {
                                        g.sidebar_alpha = v;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        div()
                            .w(px(34.0))
                            .text_size(px(12.0))
                            .text_color(c.muted)
                            .child(format!("{:.0} %", self.sidebar_alpha)),
                    )
                    .into_any_element(),
            ))
            .child(setting(
                "Acento",
                SwatchRow::new("accent", &t, accents)
                    .selected(Some(self.accent_ix))
                    .on_select({
                        let this = this.clone();
                        move |ix, _, cx| {
                            this.update(cx, |g, cx| {
                                g.accent_ix = ix;
                                cx.notify();
                            });
                        }
                    })
                    .into_any_element(),
            ));

        let popover = Popover::new("settings-pop", &t)
            .open(self.popover_open)
            .placement(Placement::BottomStart)
            .width(220.0)
            .on_dismiss({
                let this = this.clone();
                move |_, cx| {
                    this.update(cx, |g, cx| {
                        g.popover_open = false;
                        cx.notify();
                    });
                }
            })
            .trigger(
                Pill::new("pop-trigger", &t)
                    .icon(Icon::Layout)
                    .label("Layout")
                    .active(self.popover_open)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            this.update(cx, |g, cx| {
                                g.popover_open = !g.popover_open;
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(SectionLabel::new(&t, "Presets"))
            .children(WirePreset::ALL.iter().take(3).enumerate().map(|(i, p)| {
                ListRow::new(("pop", i), &t)
                    .icon(Icon::Layout)
                    .title(p.label())
                    .selected(i == 1)
                    .on_click(|_, _, _| {})
            }));

        let col2 = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(380.0))
            .child(SectionLabel::new(
                &t,
                "SegmentedControl · Toggle · Slider · Swatch",
            ))
            .child(settings)
            .child(SectionLabel::new(
                &t,
                "Popover (origen físico, Esc / clic fuera)",
            ))
            .child(popover)
            // Reserve the room the open popover covers so the layout does not jump.
            .child(div().h(px(150.0)))
            .child(SectionLabel::new(&t, "Tooltip"))
            .child(Tooltip::bubble(
                &t,
                "Facturables: 12.4M\nCaché (lecturas): 84M\nestimado",
            ));

        // --- column 3: lists + wireframes -------------------------------------------------
        let sel = self.row_sel;
        let list = {
            let this = this.clone();
            VirtualList::new("rows", 10_000, ListRow::HEIGHT, move |range, _, _| {
                range
                    .map(|ix| {
                        let (title, sub, meta, st) = Gallery::row_data(ix);
                        let this = this.clone();
                        ListRow::new(("row", ix), &t)
                            .title(title)
                            .subtitle(sub)
                            .meta(meta)
                            .status(st)
                            .selected(ix == sel)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |g, cx| {
                                    g.row_sel = ix;
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    })
                    .collect()
            })
            .track_scroll(self.scroll.clone())
            .h(px(ListRow::HEIGHT * 5.0))
        };
        let wires = div().flex().flex_row().flex_wrap().gap(px(10.0)).children(
            WirePreset::ALL.iter().enumerate().map(|(i, p)| {
                let this = this.clone();
                div()
                    .id(("wire", i))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .child(
                        Wireframe::preset(&t, *p)
                            .cell(3.0)
                            .selected(self.preset == i),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(if self.preset == i { c.text } else { c.muted })
                            .child(SharedString::from(p.label())),
                    )
                    .on_click(move |_, _, cx| {
                        this.update(cx, |g, cx| {
                            g.preset = i;
                            cx.notify();
                        });
                    })
            }),
        );
        let col3 = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(340.0))
            .child(SectionLabel::new(
                &t,
                "VirtualList · ListRow (10 000 filas)",
            ))
            .child(Card::new(&t).p(px(6.0)).child(list))
            .child(SectionLabel::new(&t, "Wireframe · presets de layout"))
            .child(wires)
            .child(SectionLabel::new(&t, "Card"))
            .child(
                div().flex().flex_row().gap(px(8.0)).children(
                    [
                        ("surface_0", CardTone::Base),
                        ("surface_1", CardTone::Raised),
                        ("surface_2", CardTone::High),
                    ]
                    .into_iter()
                    .map(|(l, tone)| {
                        Card::new(&t)
                            .tone(tone, &t)
                            .w(px(120.0))
                            .h(px(56.0))
                            .items_center()
                            .justify_center()
                            .flex()
                            .text_size(px(12.0))
                            .text_color(c.muted)
                            .child(l)
                    }),
                ),
            );

        // --- column 4: inputs + keyboard focus -----------------------------------------------
        let col4 = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(340.0))
            .child(SectionLabel::new(
                &t,
                "TextField · foco, valor, ellipsis, error",
            ))
            .child(self.name.clone())
            .child(self.long_path.clone())
            .child(self.invalid.clone())
            .child(self.secret.clone())
            .child(self.frozen.clone())
            .child(SectionLabel::new(&t, "TextArea"))
            .child(self.notes.clone())
            .child(SectionLabel::new(
                &t,
                "Select · max_w + ellipsis · >50 virtualizado",
            ))
            .child(self.select.clone())
            .child(self.select_long.clone())
            .child(SectionLabel::new(&t, "Checkbox · RadioGroup"))
            .child(
                Checkbox::new("gal-ck", &t)
                    .label("Enviar diagnósticos")
                    .checked(self.agreed)
                    .on_change({
                        let this = this.clone();
                        move |v, _, cx| {
                            this.update(cx, |g, cx| {
                                g.agreed = v;
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                RadioGroup::new("gal-rg", &t, ["Compacta", "Cómoda", "Amplia"])
                    .selected(Some(self.density))
                    .horizontal(true)
                    .on_change({
                        let this = this.clone();
                        move |ix, _, cx| {
                            this.update(cx, |g, cx| {
                                g.density = ix;
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(SectionLabel::new(
                &t,
                "Foco de teclado (Tab) · anillo accent a 2 px",
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(px(14.0))
                    .child(Button::tonal("f-btn", &t, "Button").on_click(|_, _, _| {}))
                    .child(IconButton::new("f-icon", &t, Icon::Settings).on_click(|_, _, _| {}))
                    .child(
                        Pill::new("f-pill", &t)
                            .label("Pill")
                            .active(true)
                            .on_click(|_, _, _| {}),
                    )
                    .child(Toggle::new("f-toggle", &t).on(true).on_change(|_, _, _| {}))
                    .child(
                        Button::tonal("f-off", &t, "focusable(false)")
                            .focusable(false)
                            .on_click(|_, _, _| {}),
                    ),
            );

        let radio_rows: Vec<_> = [
            ("Rapido", "Modelo pequeno, respuestas cortas"),
            ("Equilibrado", "El valor por defecto"),
            ("Profundo", "Mas razonamiento, mas tokens"),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (title, sub))| {
            let this = this.clone();
            RadioRow::new("gal-radio", ix, 3, &t)
                .selected(Some(self.radio_row))
                .on_select(move |i, _, cx| {
                    this.update(cx, |g, cx| {
                        g.radio_row = i;
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(title)
                        .child(div().text_size(px(12.0)).text_color(c.muted).child(sub)),
                )
                .into_any_element()
        })
        .collect();
        let nav_pills: Vec<_> = [
            (Icon::Terminal, "Terminal"),
            (Icon::GitBranch, "Git"),
            (
                Icon::Files,
                "Archivos con un nombre larguisimo que se corta",
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (ic, label))| {
            let this = this.clone();
            Pill::new(format!("gal-nav-{ix}"), &t)
                .full_width(true)
                .icon(ic)
                .label(label)
                .active(self.nav_row == ix)
                .on_click(move |_, _, cx| {
                    this.update(cx, |g, cx| {
                        g.nav_row = ix;
                        cx.notify();
                    });
                })
                .into_any_element()
        })
        .collect();
        let col5 = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w(px(300.0))
            .child(SectionLabel::new(
                &t,
                "TextField · set_invalid (solo borde)",
            ))
            .child(self.flagged.clone())
            .child(SectionLabel::new(
                &t,
                "SwatchRow · anillo en blanco / casi fondo",
            ))
            .child(SwatchRow::new(
                "gal-swatch",
                &t,
                [
                    rgb(0x00AAFF),
                    rgb(0xFFFFFF),
                    rgb(0xF4F4F6),
                    rgb(0x1B1B1F),
                    rgb(0x000000),
                    rgb(0x7FCB8F),
                ],
            ))
            .child(SectionLabel::new(&t, "Rotation · chevrons derivados"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(IconButton::new("gal-rot-0", &t, Icon::ChevronDown))
                    .child(
                        IconButton::new("gal-rot-180", &t, Icon::ChevronDown)
                            .rotate(Rotation::Deg180),
                    )
                    .child(IconButton::new("gal-rot-r", &t, Icon::ChevronRight))
                    .child(
                        IconButton::new("gal-rot-l", &t, Icon::ChevronRight)
                            .rotate(Rotation::Deg180),
                    ),
            )
            .child(SectionLabel::new(
                &t,
                "Pill full_width · lista de navegacion",
            ))
            .child(div().flex().flex_col().gap(px(2.0)).children(nav_pills))
            .child(SectionLabel::new(
                &t,
                "RadioRow · una parada, flechas, Espacio",
            ))
            .child(div().flex().flex_col().gap(px(4.0)).children(radio_rows))
            .child(SectionLabel::new(&t, "FocusCard"))
            .child(
                FocusCard::new("gal-fc", &t)
                    .on_click(|_, _, _| {})
                    .child("Tarjeta enfocable: Tab + Enter / Espacio"),
            )
            .child(SectionLabel::new(&t, "EmptyState"))
            .child(
                Card::new(&t).child(
                    EmptyState::new(&t, "Sin cambios")
                        .icon(Icon::GitBranch)
                        .body("El arbol de trabajo esta limpio.")
                        .action(Button::tonal("gal-es", &t, "Actualizar").on_click(|_, _, _| {})),
                ),
            );

        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .p(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(Logo::new(t.mode).size(40.0))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Selachi · ui_kit"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(c.muted)
                            .child(format!("{:?}", t.mode)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(28.0))
                    .child(col1)
                    .child(col2)
                    .child(col3)
                    .child(col4)
                    .child(col5),
            )
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") {
        Mode::Light
    } else if args.iter().any(|a| a == "oled") {
        Mode::Oled
    } else {
        Mode::Dark
    };
    let open = !args.iter().any(|a| a == "--closed");
    let rings = args.iter().any(|a| a == "--rings");
    let select_open = args.iter().any(|a| a == "--select-open");

    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
                eprintln!("fonts: {e}");
            }
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            ui_kit::init(cx);
            if rings {
                // The ring shows after keyboard focus; forcing it lets a screenshot show it without input.
                ui_kit::debug_force_focus_ring(&[
                    "f-btn",
                    "f-icon",
                    "f-pill",
                    "f-toggle",
                    "gal-ck",
                    "gal-rg-1",
                    "gal-fc",
                    "gal-radio-1",
                    "gal-select-trigger",
                    "b1",
                    "rail2",
                    "crumb",
                    "alpha",
                ]);
            }
            let bounds = Bounds::centered(None, size(px(1960.0), px(1000.0)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| Gallery::new(mode, open, cx)),
            );
            let Ok(handle) = opened else { return };
            let _ = handle.update(cx, |g, window, cx| {
                if rings {
                    g.name.read(cx).focus(window);
                }
                if select_open {
                    g.select.update(cx, |s, cx| s.open(cx));
                }
            });
            cx.activate(true);
        });
}
