use std::time::Duration;

use gpui::{AppContext as _, TestAppContext};
use panel_api::{PanelCtx, PanelZone, WidthClass};
use theme::Theme;

use crate::health::{AppStats, HISTORY};
use crate::model::testing::FakeSource;
use crate::model::{DoctorModel, SAMPLE_INTERVAL};
use crate::view::DoctorPanel;

fn new_model(cx: &mut TestAppContext) -> (gpui::Entity<DoctorModel>, std::sync::Arc<FakeSource>) {
    let src = FakeSource::new();
    let s = src.clone();
    (cx.new(move |_| DoctorModel::with_source(s)), src)
}

fn tick(cx: &mut TestAppContext, by: Duration) {
    cx.executor().advance_clock(by);
    cx.run_until_parked();
}

#[gpui::test]
async fn hidden_means_no_sampling_and_visible_means_every_second(cx: &mut TestAppContext) {
    let (model, src) = new_model(cx);
    tick(cx, Duration::from_secs(30));
    assert_eq!(src.calls(), 0);

    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(src.calls(), 1);
    tick(cx, SAMPLE_INTERVAL);
    tick(cx, SAMPLE_INTERVAL);
    assert_eq!(src.calls(), 3);

    model.update(cx, |m, cx| m.set_visible(false, cx));
    tick(cx, Duration::from_secs(60));
    assert_eq!(src.calls(), 3, "nothing after hiding");
    assert!(!model.read_with(cx, |m, _| m.is_sampling()));
}

#[gpui::test]
async fn cpu_comes_from_consecutive_samples_and_history_is_capped(cx: &mut TestAppContext) {
    let (model, _src) = new_model(cx);
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.health().cpu),
        None,
        "no baseline yet"
    );
    for _ in 0..70 {
        tick(cx, SAMPLE_INTERVAL);
    }
    let h = model.read_with(cx, |m, _| m.health().clone());
    // Fake: 0.4 s of CPU per second on 4 cores = 10 %.
    assert!((h.cpu.unwrap() - 10.0).abs() < 0.01, "{:?}", h.cpu);
    assert_eq!(
        (h.ram_history.len(), h.cpu_history.len()),
        (HISTORY, HISTORY)
    );
    assert_eq!(h.children.len(), 1);
    assert!(h.uptime.unwrap() >= Duration::from_secs(90));
}

#[gpui::test]
async fn frames_count_since_open_and_never_wake_the_ui(cx: &mut TestAppContext) {
    let (model, _src) = new_model(cx);
    // Frames painted before the doctor was opened do not count.
    for _ in 0..5 {
        model.read_with(cx, |m, _| m.record_frame(Duration::from_millis(1)));
    }
    model.update(cx, |m, cx| m.set_visible(true, cx));
    cx.run_until_parked();
    for ms in [2, 7, 3] {
        model.read_with(cx, |m, _| m.record_frame(Duration::from_millis(ms)));
    }
    tick(cx, SAMPLE_INTERVAL);
    let f = model.read_with(cx, |m, _| m.health().frames);
    assert_eq!(f.since_open, 3);
    assert_eq!(f.last, Some(Duration::from_millis(3)));
    assert_eq!(f.slowest, Some(Duration::from_millis(7)));
}

#[gpui::test]
async fn app_stats_and_db_sizes_flow_into_the_diagnosis(cx: &mut TestAppContext) {
    let (model, _src) = new_model(cx);
    model.update(cx, |m, cx| {
        m.set_db_paths(vec![
            ("selachi.db".into(), "a.db".into()),
            ("state.db".into(), "b.db".into()),
        ]);
        m.set_app_stats(
            AppStats {
                shells: Some(2),
                agents: Some(1),
                grammars: vec!["rust".into()],
                tree_tracked: 0,
            },
            cx,
        );
        m.set_visible(true, cx);
    });
    cx.run_until_parked();
    let md = model.read_with(cx, |m, _| m.markdown("9.9"));
    for needle in [
        "(9.9)",
        "**Shells / agentes:** 2 / 1",
        "**selachi.db:** 1 KB",
        "**state.db:** 1 KB",
        "rust",
    ] {
        assert!(md.contains(needle), "missing {needle:?}\n{md}");
    }
}

#[gpui::test]
async fn the_panel_paints_in_both_widths_and_starts_sampling(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.text_system().add_fonts(theme::fonts::embedded()).ok();
    });
    let (model, src) = new_model(cx);
    let ctx = |w| PanelCtx::new(PanelZone::Right, w, Theme::light());
    let (panel, vcx) = {
        let m = model.clone();
        cx.add_window_view(move |_, cx| {
            DoctorPanel::new(ctx(WidthClass::Regular), m, "0.0.1".into(), cx)
        })
    };
    vcx.run_until_parked();
    assert!(src.calls() >= 1 && model.read_with(vcx, |m, _| m.is_sampling()));
    panel.update(vcx, |p, cx| p.retarget(ctx(WidthClass::Compact), cx));
    vcx.run_until_parked();
    assert!(panel.read_with(vcx, |p, _| p.is_compact()));
}

#[gpui::test]
async fn the_copy_button_puts_the_markdown_diagnosis_on_the_clipboard(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.text_system().add_fonts(theme::fonts::embedded()).ok();
    });
    let (model, _src) = new_model(cx);
    let (panel, vcx) = {
        let m = model;
        ui_kit::testing::open_window(cx, move |_, cx| {
            DoctorPanel::new(
                PanelCtx::new(PanelZone::Right, WidthClass::Regular, Theme::dark()),
                m,
                "9.9".into(),
                cx,
            )
        })
    };
    vcx.run_until_parked();
    assert!(
        vcx.read_from_clipboard().is_none(),
        "nothing is copied until the button is pressed"
    );
    ui_kit::testing::click_id(vcx, "doctor-copy");
    let copied = vcx
        .read_from_clipboard()
        .and_then(|c| c.text())
        .expect("the diagnosis is on the clipboard");
    assert!(copied.contains("(9.9)"), "{copied}");
    assert!(
        panel.read_with(vcx, |p, _| p.is_copied()),
        "the button says it copied"
    );
}
