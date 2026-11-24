//! The palette through a real (test) window: keys travel through the keymap into the shared
//! `ui_kit::TextField` and come out as [`PaletteEvent`]s. Nothing touches the desktop.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, FocusHandle, IntoElement, Render, Subscription, TestAppContext,
    VisualTestContext, Window, div, prelude::*,
};
use store::Param;
use theme::Theme;
use ui_kit::testing::{self, press, type_text};

use crate::{
    ActionEntry, ActionsSource, CommandPalette, FormSpec, ItemAction, OpenContext, PaletteEvent,
    PaletteItem, PaletteSource, Scope, SourceQuery, command_palette,
};

type Events = Rc<RefCell<Vec<PaletteEvent>>>;

struct Host {
    focus: FocusHandle,
    palette: Entity<CommandPalette>,
    _sub: Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The palette is the last child of a full-size root, like in the shell.
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .child(self.palette.clone())
    }
}

/// One item with a primary and an alternative action, and one with a parameter form.
struct Fixture;

impl PaletteSource for Fixture {
    fn id(&self) -> &'static str {
        "fixture"
    }
    fn title(&self) -> &'static str {
        "Fixture"
    }
    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Snippets]
    }
    fn query(&self, q: &SourceQuery<'_>) -> Vec<PaletteItem> {
        if !q.text.is_empty() && !"deploy".contains(q.text.trim()) {
            return Vec::new();
        }
        let both = PaletteItem::new(
            "fixture:both",
            "Deploy",
            ItemAction::emit("Insertar", PaletteEvent::InsertText("deploy".into())),
        )
        .alt(ItemAction::emit(
            "Ejecutar",
            PaletteEvent::RunCommand {
                line: "deploy".into(),
                cwd: None,
                new_tab: false,
            },
        ));
        let form = PaletteItem::new(
            "fixture:form",
            "Deploy a entorno",
            ItemAction::form(
                "Insertar",
                FormSpec::snippet(
                    7,
                    "Deploy a entorno",
                    "deploy {env|dev|prod}",
                    vec![Param {
                        name: "env".into(),
                        default: Some("dev".into()),
                        options: vec!["dev".into(), "prod".into()],
                    }],
                    None,
                ),
            ),
        );
        vec![both, form]
    }
}

fn entries() -> Vec<ActionEntry> {
    vec![
        ActionEntry::new("tab.new", "Nueva pestaña"),
        ActionEntry::new("tab.close", "Cerrar pestaña"),
        ActionEntry::new("settings.open", "Ajustes"),
    ]
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    initial: &str,
) -> (
    Entity<CommandPalette>,
    FocusHandle,
    &'a mut VisualTestContext,
    Events,
) {
    let events: Events = Rc::default();
    let sink = events.clone();
    let (host, vcx) = testing::open_window(cx, move |window, cx| {
        let sources: Vec<std::sync::Arc<dyn PaletteSource>> = vec![
            std::sync::Arc::new(ActionsSource::new(entries())),
            std::sync::Arc::new(Fixture),
        ];
        let palette = command_palette(Theme::dark(), None, sources, cx);
        let sub = cx.subscribe(&palette, move |_: &mut Host, _, ev: &PaletteEvent, _| {
            sink.borrow_mut().push(ev.clone())
        });
        let focus = cx.focus_handle();
        window.focus(&focus);
        Host {
            focus,
            palette,
            _sub: sub,
        }
    });
    let (palette, focus) = host.read_with(vcx, |h, _| (h.palette.clone(), h.focus.clone()));
    let ctx = OpenContext {
        initial: initial.to_string(),
        ..OpenContext::default()
    };
    vcx.update(|window, cx| palette.update(cx, |p, cx| p.open(ctx, window, cx)));
    vcx.run_until_parked();
    (palette, focus, vcx, events)
}

fn input_text(palette: &Entity<CommandPalette>, vcx: &VisualTestContext) -> String {
    palette.read_with(vcx, |p, cx| p.input_text(cx))
}

#[gpui::test]
fn open_type_enter_emits_the_action_and_closes(cx: &mut TestAppContext) {
    let (palette, focus, vcx, events) = open(cx, "");
    assert!(palette.read_with(vcx, |p, _| p.is_open()));
    type_text(vcx, "ajus");
    vcx.run_until_parked();
    press(vcx, "enter");
    vcx.run_until_parked();
    assert_eq!(
        *events.borrow(),
        vec![
            PaletteEvent::Action("settings.open".into()),
            PaletteEvent::Closed
        ]
    );
    assert!(!palette.read_with(vcx, |p, _| p.is_open()));
    assert!(
        testing::is_focused(vcx, &focus),
        "focus goes back to what had it"
    );
}

#[gpui::test]
fn down_moves_the_selection_and_up_returns(cx: &mut TestAppContext) {
    let (_palette, _, vcx, events) = open(cx, ">");
    press(vcx, "down down up");
    vcx.run_until_parked();
    press(vcx, "enter");
    vcx.run_until_parked();
    // `>` lists the actions in order: down, down, up lands on the second.
    assert_eq!(
        events.borrow().first(),
        Some(&PaletteEvent::Action("tab.close".into()))
    );
}

#[gpui::test]
fn escape_closes_without_emitting_anything_else(cx: &mut TestAppContext) {
    let (palette, focus, vcx, events) = open(cx, "");
    type_text(vcx, "nue");
    press(vcx, "escape");
    vcx.run_until_parked();
    assert_eq!(*events.borrow(), vec![PaletteEvent::Closed]);
    assert!(!palette.read_with(vcx, |p, _| p.is_open()));
    assert!(testing::is_focused(vcx, &focus));
}

#[gpui::test]
fn ctrl_enter_runs_the_alternative_action(cx: &mut TestAppContext) {
    let (_palette, _, vcx, events) = open(cx, "$");
    press(vcx, "ctrl-enter");
    vcx.run_until_parked();
    assert_eq!(
        events.borrow().first(),
        Some(&PaletteEvent::RunCommand {
            line: "deploy".into(),
            cwd: None,
            new_tab: false
        })
    );
    // Plain Enter is the primary action.
    let (_palette, _, vcx, events) = open(cx, "$");
    press(vcx, "enter");
    vcx.run_until_parked();
    assert_eq!(
        events.borrow().first(),
        Some(&PaletteEvent::InsertText("deploy".into()))
    );
}

#[gpui::test]
fn tab_and_shift_tab_cycle_the_scope_and_keep_the_focus(cx: &mut TestAppContext) {
    let (palette, _, vcx, _) = open(cx, "");
    press(vcx, "tab");
    assert_eq!(input_text(&palette, vcx), ">");
    press(vcx, "tab");
    assert_eq!(input_text(&palette, vcx), "#");
    press(vcx, "shift-tab shift-tab");
    assert_eq!(input_text(&palette, vcx), "");
    // Typing still reaches the field: focus never left it.
    type_text(vcx, "nue");
    assert_eq!(input_text(&palette, vcx), "nue");
}

#[gpui::test]
fn form_choice_cycles_with_the_arrows_and_ctrl_enter_runs_it(cx: &mut TestAppContext) {
    let (palette, _, vcx, events) = open(cx, "$");
    // The second item has the parameter form.
    press(vcx, "down enter");
    vcx.run_until_parked();
    assert!(
        palette.read_with(vcx, |p, _| p.in_form_for_test()),
        "Enter on a form item opens the form"
    );
    // The default is `dev`; the arrow cycles the options instead of moving a caret.
    press(vcx, "right");
    press(vcx, "ctrl-enter");
    vcx.run_until_parked();
    assert_eq!(
        events.borrow().first(),
        Some(&PaletteEvent::RunCommand {
            line: "deploy prod".into(),
            cwd: None,
            new_tab: false
        })
    );
}

#[gpui::test]
fn escape_in_a_form_goes_back_to_the_list_first(cx: &mut TestAppContext) {
    let (palette, _, vcx, events) = open(cx, "$");
    press(vcx, "down enter");
    vcx.run_until_parked();
    press(vcx, "escape");
    vcx.run_until_parked();
    assert!(palette.read_with(vcx, |p, _| p.is_open() && !p.in_form_for_test()));
    assert!(events.borrow().is_empty());
    press(vcx, "escape");
    vcx.run_until_parked();
    assert_eq!(*events.borrow(), vec![PaletteEvent::Closed]);
}
