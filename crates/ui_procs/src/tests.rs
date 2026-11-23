//! Behavioural tests over a fake source: sampling gate, confirmation flow, layout switching.

use std::time::Duration;

use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use panel_api::{PanelCtx, PanelZone, WidthClass};
use theme::Theme;

use crate::model::OrphanSummary;
use crate::plan::PlanKind;
use crate::state::testing::FakeSource;
use crate::state::{Dialog, ProcsModel, SAMPLE_INTERVAL, SummaryChanged};
use crate::view::ProcsPanel;

fn new_model(cx: &mut TestAppContext) -> (gpui::Entity<ProcsModel>, std::sync::Arc<FakeSource>) {
    let src = FakeSource::new();
    let model = cx.new(|_| ProcsModel::new(src.clone()));
    (model, src)
}

fn tick(cx: &mut TestAppContext, by: Duration) {
    cx.executor().advance_clock(by);
    cx.run_until_parked();
}

#[gpui::test]
async fn nothing_runs_while_hidden(cx: &mut TestAppContext) {
    let (_model, src) = new_model(cx);
    tick(cx, Duration::from_secs(60));
    assert_eq!(src.collects(), 0, "a hidden panel must not sample");
}

#[gpui::test]
async fn sampling_runs_every_two_seconds_only_while_visible(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.collects(), 1, "immediate sample on show");
    assert!(model.read_with(cx, |m, _| m.is_sampling()));
    tick(cx, SAMPLE_INTERVAL);
    assert_eq!(src.collects(), 2);
    tick(cx, SAMPLE_INTERVAL);
    assert_eq!(src.collects(), 3);

    model.update(cx, |m, cx| m.set_visible(false, cx));
    assert!(!model.read_with(cx, |m, _| m.is_sampling()));
    let after_hide = src.collects();
    tick(cx, Duration::from_secs(120));
    assert_eq!(src.collects(), after_hide, "no sampling after hiding");
    assert_eq!(src.resets.load(std::sync::atomic::Ordering::SeqCst), 1, "CPU baseline dropped");

    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.collects(), after_hide + 1, "shows again with a fresh sample");
}

#[gpui::test]
async fn last_view_dropping_stops_sampling(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| {
        m.attach_view();
        m.attach_view();
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    model.update(cx, |m, cx| m.detach_view(cx));
    assert!(model.read_with(cx, |m, _| m.is_sampling()), "one view is still open");
    model.update(cx, |m, cx| m.detach_view(cx));
    let n = src.collects();
    tick(cx, Duration::from_secs(30));
    assert_eq!(src.collects(), n);
}

#[gpui::test]
async fn job_events_refresh_even_when_hidden_but_are_throttled(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.on_job_event(cx));
    cx.run_until_parked();
    assert_eq!(src.collects(), 1);
    // A burst of events inside the throttle window collapses into one trailing refresh.
    for _ in 0..20 {
        model.update(cx, |m, cx| m.on_job_event(cx));
    }
    cx.run_until_parked();
    assert_eq!(src.collects(), 1, "throttled");
    tick(cx, Duration::from_secs(3));
    assert_eq!(src.collects(), 2, "one trailing refresh");
    assert!(!model.read_with(cx, |m, _| m.is_sampling()), "events never start the loop");
    assert_eq!(model.read_with(cx, |m, _| m.orphan_summary().count), 2);
}

#[gpui::test]
async fn summary_event_fires_when_orphans_change(cx: &mut TestAppContext) {
    let (model, _src) = new_model(cx);
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = seen.clone();
    let _sub = cx.update(|cx| {
        cx.subscribe(&model, move |_, ev: &SummaryChanged, _| sink.borrow_mut().push(ev.0))
    });
    model.update(cx, |m, cx| m.refresh(cx));
    cx.run_until_parked();
    let got = seen.borrow().clone();
    assert_eq!(got, [OrphanSummary { count: 2, ram_bytes: 435 * 1024 * 1024 }]);
    model.update(cx, |m, cx| m.refresh(cx));
    cx.run_until_parked();
    assert_eq!(seen.borrow().len(), 1, "unchanged summary is not re-announced");
}

#[gpui::test]
async fn nothing_is_terminated_without_confirmation(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.refresh(cx));
    cx.run_until_parked();

    // Confirming with no dialog does nothing.
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(src.terminations(), 0);

    // Requesting only builds a preview.
    model.update(cx, |m, cx| m.request_clean_orphans(cx));
    cx.run_until_parked();
    assert_eq!(src.terminations(), 0, "the preview must not terminate");
    let plan = model.read_with(cx, |m, _| match m.dialog() {
        Dialog::Confirm(p) => p.clone(),
        other => panic!("expected a confirmation, got {other:?}"),
    });
    assert_eq!(plan.kind, PlanKind::Orphans);
    assert_eq!(plan.items.iter().map(|i| i.pid).collect::<Vec<_>>(), [102, 103]);

    // Cancel discards it.
    model.update(cx, |m, cx| m.cancel(cx));
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(src.terminations(), 0, "cancelled dialogs cannot be confirmed");
    assert_eq!(model.read_with(cx, |m, _| m.dialog().clone()), Dialog::None);
}

#[gpui::test]
async fn confirm_terminates_exactly_the_previewed_roots(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.request_clean_orphans(cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(*src.terminated.lock().unwrap(), vec![vec![102]]);
    match model.read_with(cx, |m, _| m.dialog().clone()) {
        Dialog::Done { result, .. } => assert_eq!(result.stopped(), 1),
        other => panic!("expected Done, got {other:?}"),
    }
    // A second confirm after the run is inert.
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(src.terminations(), 1);
}

#[gpui::test]
async fn stop_one_process_previews_its_tree_and_respects_the_safety_layer(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.request_stop(102, cx));
    cx.run_until_parked();
    let items = model.read_with(cx, |m, _| match m.dialog() {
        Dialog::Confirm(p) => p.items.iter().map(|i| i.pid).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    });
    assert_eq!(items, [102, 103], "the process and its child");
    model.update(cx, |m, cx| m.cancel(cx));

    // A refused root (critical process) can be previewed but never confirmed.
    src.refuse.lock().unwrap().push(102);
    model.update(cx, |m, cx| m.request_stop(102, cx));
    cx.run_until_parked();
    model.update(cx, |m, cx| m.confirm(cx));
    cx.run_until_parked();
    assert_eq!(src.terminations(), 0);
    model.read_with(cx, |m, _| match m.dialog() {
        Dialog::Confirm(p) => assert!(p.is_empty() && p.refused.len() == 1),
        other => panic!("{other:?}"),
    });
}

#[gpui::test]
async fn a_vanished_process_gives_a_notice_not_a_kill(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    model.update(cx, |m, cx| m.request_stop(9_999, cx));
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| matches!(m.dialog(), Dialog::Notice(_))));
    assert_eq!(src.terminations(), 0);
}

fn ctx(width: WidthClass) -> PanelCtx {
    PanelCtx::new(PanelZone::Right, width, Theme::dark())
}

/// The panel over a fake source in a real test window (fonts loaded, one frame painted).
fn open_panel(cx: &mut TestAppContext) -> (gpui::Entity<ProcsModel>, std::sync::Arc<FakeSource>, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.text_system().add_fonts(theme::fonts::embedded()).ok();
    });
    let (model, src) = new_model(cx);
    let (_panel, vcx) = {
        let model = model.clone();
        ui_kit::testing::open_window(cx, move |_, cx| ProcsPanel::new(ctx(WidthClass::Regular), model, cx))
    };
    vcx.run_until_parked();
    (model, src, vcx)
}

#[gpui::test]
async fn the_buttons_gate_every_termination_behind_the_preview(cx: &mut TestAppContext) {
    use ui_kit::testing::click_id;
    let (model, src, vcx) = open_panel(cx);
    // Two orphans are in the fake tree: the header button is enabled and only opens a preview.
    click_id(vcx, "procs-clean");
    vcx.run_until_parked();
    assert!(model.read_with(vcx, |m, _| matches!(m.dialog(), Dialog::Confirm(_))), "the click opens the confirmation");
    assert_eq!(src.terminations(), 0, "clicking \"Limpiar huérfanos\" must not terminate anything");

    // Cancel discards the plan; there is nothing left to confirm.
    click_id(vcx, "dlg-cancel");
    vcx.run_until_parked();
    assert_eq!(model.read_with(vcx, |m, _| m.dialog().clone()), Dialog::None);
    assert_eq!(src.terminations(), 0);

    // Preview again and confirm: exactly the previewed roots are stopped.
    click_id(vcx, "procs-clean");
    vcx.run_until_parked();
    click_id(vcx, "dlg-confirm");
    vcx.run_until_parked();
    assert_eq!(*src.terminated.lock().unwrap(), vec![vec![102]]);
    assert!(model.read_with(vcx, |m, _| matches!(m.dialog(), Dialog::Done { .. })));
}

#[gpui::test]
async fn a_refused_plan_has_a_disabled_confirm_button(cx: &mut TestAppContext) {
    use ui_kit::testing::click_id;
    let (model, src, vcx) = open_panel(cx);
    // The only root of the preview is refused by the safety layer: the plan is empty.
    src.refuse.lock().unwrap().push(102);
    model.update(vcx, |m, cx| m.request_stop(102, cx));
    vcx.run_until_parked();
    assert!(model.read_with(vcx, |m, _| matches!(m.dialog(), Dialog::Confirm(p) if p.is_empty())));
    click_id(vcx, "dlg-confirm");
    vcx.run_until_parked();
    assert_eq!(src.terminations(), 0, "a disabled confirm button does nothing when clicked");
    assert!(model.read_with(vcx, |m, _| matches!(m.dialog(), Dialog::Confirm(_))), "the dialog stays open");
}

#[gpui::test]
async fn compact_layout_switches_with_the_width_class_and_renders(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.text_system().add_fonts(theme::fonts::embedded()).ok();
    });
    let (model, src) = new_model(cx);
    let (panel, vcx): (_, &mut VisualTestContext) = {
        let model = model.clone();
        cx.add_window_view(move |_, cx| ProcsPanel::new(ctx(WidthClass::Regular), model, cx))
    };
    vcx.run_until_parked();
    assert!(!panel.read_with(vcx, |p, _| p.is_compact()));
    assert!(src.collects() >= 1, "painting the panel starts sampling");
    assert!(model.read_with(vcx, |m, _| m.is_sampling()));

    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Compact), cx));
    vcx.run_until_parked();
    assert!(panel.read_with(vcx, |p, _| p.is_compact()));

    // Both widths and both tabs paint without panicking, with a dialog open too.
    model.update(vcx, |m, cx| m.request_clean_orphans(cx));
    vcx.run_until_parked();
    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Regular), cx));
    vcx.run_until_parked();
}

#[test]
fn metrics_hide_cpu_and_uptime_when_compact() {
    use crate::view::metrics_text;
    let up = Some(Duration::from_secs(125));
    assert_eq!(metrics_text(340 * 1024 * 1024, Some(3.4), up, false), "340 MB · 3,4% · 2m");
    assert_eq!(metrics_text(340 * 1024 * 1024, Some(3.4), up, true), "340 MB");
    assert_eq!(metrics_text(0, None, None, false), "0 KB · — · —");
}
