//! Text measuring contract of the vendored gpui `StyledText`: the lines that are painted always
//! belong to the final bounds of the element, whatever width taffy asked for while it was
//! measuring (min-content, max-content, a tiny probe, a cached answer).
//!
//! The test text system gives every glyph `0.6 em`, so widths are exact: with `text_size(10)` a
//! character is 6 px wide.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, IntoElement, Render, StyledText, TextLayout, Window, div, prelude::*, px};

use crate::testing;

const CH: f32 = 6.0;

type Layouts = Rc<RefCell<Vec<(&'static str, TextLayout)>>>;
type Build = Box<dyn Fn(&Layouts) -> gpui::AnyElement>;

struct Host {
    layouts: Layouts,
    build: Build,
}

impl Render for Host {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.layouts.borrow_mut().clear();
        (self.build)(&self.layouts)
    }
}

/// A `StyledText` whose layout is recorded under `name`.
fn text(layouts: &Layouts, name: &'static str, s: &str) -> StyledText {
    let t = StyledText::new(s.to_string());
    layouts.borrow_mut().push((name, t.layout().clone()));
    t
}

fn run(
    cx: &mut gpui::TestAppContext,
    build: impl Fn(&Layouts) -> gpui::AnyElement + 'static,
) -> Layouts {
    let layouts: Layouts = Rc::default();
    let l = layouts.clone();
    let (_v, vcx) = testing::open_window(cx, move |_, _| Host {
        layouts: l,
        build: Box::new(build),
    });
    vcx.run_until_parked();
    layouts
}

fn find(layouts: &Layouts, name: &str) -> TextLayout {
    layouts
        .borrow()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| l.clone())
        .unwrap_or_else(|| panic!("no layout `{name}`"))
}

/// Painted rows of a layout (soft wraps included).
fn rows(l: &TextLayout) -> Vec<String> {
    l.wrapped_text().split('\n').map(str::to_string).collect()
}

/// Width of the widest painted row.
fn painted_width(l: &TextLayout) -> f32 {
    rows(l)
        .iter()
        .map(|r| r.trim_end().chars().count() as f32 * CH)
        .fold(0.0, f32::max)
}

fn assert_inside(l: &TextLayout, what: &str) {
    let b = l.bounds();
    let w = painted_width(l);
    assert!(
        w <= f32::from(b.size.width) + 1.0,
        "{what}: painted {w} px of text in a {} px box: {:?}",
        f32::from(b.size.width),
        rows(l)
    );
}

fn small(d: gpui::Div) -> gpui::Div {
    d.text_size(px(10.0))
}

/// `group_with_action`: a title that must stay on one line, an action on the right.
#[gpui::test]
fn header_title_next_to_an_action_stays_on_one_line(cx: &mut gpui::TestAppContext) {
    for width in [260.0_f32, 360.0, 520.0, 1000.0] {
        let ls = run(cx, move |ls| {
            small(div().w(px(width)).flex().flex_col())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .min_w_0()
                        .px(px(4.0))
                        .child(
                            div()
                                .min_w_0()
                                .child(text(ls, "title", "Perfiles de shell")),
                        )
                        .child(div().w(px(110.0)).h(px(20.0))),
                )
                .into_any_element()
        });
        let t = find(&ls, "title");
        assert_eq!(rows(&t).len(), 1, "width {width}: {:?}", rows(&t));
        assert_inside(&t, "header title");
    }
}

/// The same header without the `min_w_0` workaround.
#[gpui::test]
fn header_title_without_workarounds_stays_on_one_line(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(400.0)).flex().flex_col())
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().child(text(ls, "title", "CLI detectados")))
                    .child(div().w(px(110.0)).h(px(20.0))),
            )
            .into_any_element()
    });
    assert_eq!(rows(&find(&ls, "title")).len(), 1);
}

/// A title in a flex item squeezed to almost nothing by a wide neighbour: it wraps inside what is
/// left but never paints wider than its box.
#[gpui::test]
fn squeezed_wrapping_text_never_paints_wider_than_its_box(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(300.0)).flex())
            .child(div().flex_1().min_w_0().child(text(
                ls,
                "t",
                "Permitir delegar a otros agentes en paralelo",
            )))
            .child(div().flex_none().w(px(270.0)).h(px(20.0)))
            .into_any_element()
    });
    assert_inside(&find(&ls, "t"), "squeezed text");
}

/// `nowrap + ellipsis` inside a narrow flex item gets its ellipsis even though the first
/// (max-content) measure of the text had no width limit.
#[gpui::test]
fn nowrap_text_in_a_narrow_flex_item_gets_an_ellipsis(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(150.0)).flex())
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    div()
                        .w_full()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(text(ls, "t", "Endurecer la autenticación del servicio web")),
                ),
            )
            .into_any_element()
    });
    let t = find(&ls, "t");
    let r = rows(&t);
    assert_eq!(r.len(), 1);
    assert!(r[0].ends_with('…'), "no ellipsis: {r:?}");
    assert_inside(&t, "truncated text");
}

/// The common `.truncate()` pattern in a fixed width box.
#[gpui::test]
fn truncate_class_ellipsizes_in_a_fixed_width_box(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(120.0)))
            .child(div().truncate().child(text(
                ls,
                "t",
                "1 · solo el agente principal de la sesión",
            )))
            .into_any_element()
    });
    let t = find(&ls, "t");
    assert!(rows(&t)[0].ends_with('…'), "{:?}", rows(&t));
    assert_inside(&t, "select label");
}

/// Wide boxes keep the text as is (no ellipsis, no wrap).
#[gpui::test]
fn nowrap_text_that_fits_is_left_alone(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(400.0)))
            .child(div().truncate().child(text(ls, "t", "Ajustes")))
            .into_any_element()
    });
    assert_eq!(rows(&find(&ls, "t")), vec!["Ajustes".to_string()]);
}

/// A description wraps inside its column at many widths, beside a fixed control.
#[gpui::test]
fn description_wraps_inside_its_column_at_every_width(cx: &mut gpui::TestAppContext) {
    let desc = "Permite que el agente principal reparta trabajo entre otros agentes y ejecutables de la lista";
    for width in [200.0_f32, 233.5, 301.25, 420.0, 640.0, 900.0] {
        let ls = run(cx, move |ls| {
            small(div().w(px(width)).flex())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(text(ls, "label", "Permitir delegar"))
                        .child(text(ls, "desc", desc)),
                )
                .child(div().flex_none().ml(px(16.0)).w(px(80.0)).h(px(24.0)))
                .into_any_element()
        });
        let col_w = width - 16.0 - 80.0;
        let d = find(&ls, "desc");
        assert!(
            f32::from(d.bounds().size.width) <= col_w + 1.0,
            "width {width}: column {col_w} but box {}",
            f32::from(d.bounds().size.width)
        );
        assert_inside(&d, "description");
        assert_inside(&find(&ls, "label"), "label");
        let lines = rows(&d).len();
        // A text that wraps reports the height of every row it paints.
        let expected = lines as f32 * f32::from(d.line_height());
        assert!(
            (f32::from(d.bounds().size.height) - expected).abs() < 1.0,
            "width {width}: height {} for {lines} rows",
            f32::from(d.bounds().size.height)
        );
    }
}

/// A text that fits its final box paints on one line.
#[gpui::test]
fn text_that_fits_the_final_box_stays_on_one_line(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(600.0)))
            .child(text(
                ls,
                "t",
                "una descripcion de largo medio que cabe en una linea",
            ))
            .into_any_element()
    });
    assert_eq!(rows(&find(&ls, "t")).len(), 1);
}

/// The real settings page skeleton: nav + scrolling content column with `items_center`, a
/// `max_w` page and a header row. Taffy probes the title with a definite width of 0 here.
#[gpui::test]
fn header_title_in_a_scrolling_page_stays_on_one_line(cx: &mut gpui::TestAppContext) {
    for width in [760.0_f32, 1000.0, 1250.0, 1500.0] {
        let ls = run(cx, move |ls| {
            small(div().w(px(width)).h(px(600.0)).flex().flex_row())
                .child(div().w(px(220.0)).h_full().flex_none())
                .child(
                    div()
                        .id("scroll")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .h_full()
                        .overflow_hidden()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .max_w(px(860.0))
                                        .flex()
                                        .flex_col()
                                        .gap(px(20.0))
                                        .px(px(32.0))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap(px(8.0))
                                                .w_full()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_row()
                                                        .items_center()
                                                        .justify_between()
                                                        .w_full()
                                                        .min_w_0()
                                                        .px(px(4.0))
                                                        .child(div().min_w_0().child(text(
                                                            ls,
                                                            "title",
                                                            "CLI detectados",
                                                        )))
                                                        .child(div().child(text(
                                                            ls,
                                                            "action",
                                                            "Volver a comprobar",
                                                        ))),
                                                ),
                                        ),
                                ),
                        ),
                )
                .into_any_element()
        });
        let t = find(&ls, "title");
        assert_eq!(rows(&t).len(), 1, "width {width}: {:?}", rows(&t));
        assert_inside(&t, "header title");
        assert_eq!(rows(&find(&ls, "action")).len(), 1);
    }
}

fn fit(wraps: bool, truncates: bool) -> gpui::ShapeFit {
    gpui::ShapeFit {
        wraps,
        truncates,
        measured: true,
        content_width: px(100.3),
        widest_unwrapped: px(100.3),
        has_wraps: false,
        wrap_width: None,
        truncate_width: None,
        truncated: false,
    }
}

#[test]
fn a_box_half_a_pixel_narrower_than_its_text_keeps_the_text_on_one_line() {
    let f = fit(true, false);
    assert!(!f.is_stale_for(px(99.8), 1.0));
    assert!(!f.is_stale_for(px(100.0), 1.0));
    assert!(!f.is_stale_for(px(100.3), 1.0));
    // A real overflow is not tolerated.
    assert!(f.is_stale_for(px(60.0), 1.0));
}

#[test]
fn wrapped_text_is_reflowed_when_its_box_got_wider() {
    let f = gpui::ShapeFit {
        content_width: px(40.0),
        widest_unwrapped: px(160.0),
        has_wraps: true,
        wrap_width: Some(px(40.0)),
        ..fit(true, false)
    };
    assert!(f.is_stale_for(px(400.0), 1.0), "widening must reshape");
    assert!(!f.is_stale_for(px(40.5), 1.0), "within the tolerance");
    assert!(f.is_stale_for(px(20.0), 1.0), "narrowing must reshape");
}

#[test]
fn short_text_shaped_for_a_wider_width_is_left_alone() {
    let f = gpui::ShapeFit {
        content_width: px(60.0),
        widest_unwrapped: px(60.0),
        wrap_width: Some(px(500.0)),
        ..fit(true, false)
    };
    assert!(!f.is_stale_for(px(200.0), 1.0));
}

#[test]
fn nowrap_text_without_truncation_never_changes() {
    let f = gpui::ShapeFit {
        content_width: px(500.0),
        widest_unwrapped: px(500.0),
        ..fit(false, false)
    };
    assert!(!f.is_stale_for(px(100.0), 1.0));
    assert!(!f.is_stale_for(px(900.0), 1.0));
}

#[test]
fn nowrap_text_with_an_ellipsis_follows_its_final_box() {
    let full = gpui::ShapeFit {
        content_width: px(500.0),
        widest_unwrapped: px(500.0),
        ..fit(false, true)
    };
    assert!(
        full.is_stale_for(px(100.0), 1.0),
        "measured without a limit"
    );
    assert!(!full.is_stale_for(px(600.0), 1.0), "it fits");
    let cut = gpui::ShapeFit {
        content_width: px(80.0),
        widest_unwrapped: px(80.0),
        truncate_width: Some(px(80.0)),
        truncated: true,
        ..fit(false, true)
    };
    assert!(
        cut.is_stale_for(px(300.0), 1.0),
        "would hide text that fits"
    );
    assert!(!cut.is_stale_for(px(80.5), 1.0));
}

#[test]
fn an_unmeasured_text_is_always_shaped() {
    let f = gpui::ShapeFit {
        measured: false,
        ..fit(true, false)
    };
    assert!(f.is_stale_for(px(100.0), 1.0));
}

/// `line_clamp` still limits the rows after a reshape to the final width (what is past the last
/// row is clipped by `overflow_hidden`; with an ellipsis it is cut with `…`).
#[gpui::test]
fn line_clamp_is_respected(cx: &mut gpui::TestAppContext) {
    let long = "uno dos tres cuatro cinco seis siete ocho nueve diez once doce trece";
    let ls = run(cx, move |ls| {
        small(div().w(px(100.0)))
            .child(div().line_clamp(2).child(text(ls, "t", long)))
            .child(
                div()
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(text(ls, "e", long)),
            )
            .into_any_element()
    });
    assert!(rows(&find(&ls, "t")).len() <= 2);
    let e = find(&ls, "e");
    assert!(rows(&e).len() <= 2, "{:?}", rows(&e));
    assert!(e.wrapped_text().ends_with('…'), "{:?}", rows(&e));
}

/// The probe sees what was painted (used by the settings layout test).
#[gpui::test]
fn probe_reports_painted_text(cx: &mut gpui::TestAppContext) {
    gpui::text_probe::start();
    let _ = run(cx, |ls| {
        small(div().w(px(80.0)))
            .child(text(ls, "t", "uno dos tres cuatro cinco"))
            .into_any_element()
    });
    let seen = gpui::text_probe::take();
    let t = seen.last().expect("a text was painted");
    assert!(t.rows > 1);
    assert!(f32::from(t.painted_width) <= 81.0);
}

/// A text item in a flex row needs no `min_w_0` workaround: it shrinks and wraps inside its row.
#[gpui::test]
fn text_in_a_flex_row_does_not_need_min_w_0(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(200.0)).flex().flex_row())
            .child(div().flex_1().child(text(
                ls,
                "t",
                "una nota larga que debe partirse dentro de su columna sin salirse",
            )))
            .child(div().flex_none().w(px(40.0)).h(px(10.0)))
            .into_any_element()
    });
    let t = find(&ls, "t");
    assert!(
        f32::from(t.bounds().size.width) <= 160.0 + 1.0,
        "{:?}",
        t.bounds()
    );
    assert!(rows(&t).len() > 1);
    assert_inside(&t, "flex item");
}

// ---- min-content = the widest unbreakable word -------------------------------------------------

fn words(text: &str) -> Vec<String> {
    gpui::min_content_segments(text)
        .into_iter()
        .map(|r| text[r].to_string())
        .collect()
}

/// The break opportunities are the line wrapper's: spaces, and non-word characters.
#[test]
fn min_content_segments_follow_the_line_wrapper() {
    assert_eq!(words("Escribe un número entero"), ["Escribe ", "un ", "número ", "entero"]);
    assert_eq!(words("Próximamente"), ["Próximamente"]);
    assert_eq!(words("  ab cd"), ["  ab ", "cd"]);
    assert_eq!(words("pre-commit a_b.c"), ["pre-commit ", "a_b.c"]);
    assert_eq!(words("src/lib"), ["src", "/lib"]);
    assert_eq!(words(""), [""]);
}

/// A pill-like label in a row squeezed below its width keeps its whole word: it never turns into
/// a column of letters (a min-content of 0 let taffy shrink it to nothing).
#[gpui::test]
fn a_single_word_item_in_a_squeezed_row_keeps_its_word_on_one_row(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(60.0)).flex().flex_row())
            .child(div().child(text(ls, "pill", "Próximamente")))
            .child(div().flex_none().w(px(30.0)).h(px(10.0)))
            .into_any_element()
    });
    let t = find(&ls, "pill");
    assert_eq!(rows(&t), vec!["Próximamente".to_string()]);
    // min-content = the word: 12 chars
    assert!(
        f32::from(t.bounds().size.width) >= 12.0 * CH - 1.0,
        "{:?}",
        t.bounds()
    );
}

/// An error text under a field in a narrow slot wraps between words, inside the slot.
#[gpui::test]
fn error_text_under_a_narrow_field_wraps_between_words(cx: &mut gpui::TestAppContext) {
    gpui::text_probe::start();
    let ls = run(cx, |ls| {
        small(div().w(px(90.0)).flex().flex_col())
            .child(div().h(px(10.0)))
            .child(text(ls, "err", "Escribe un número entero"))
            .into_any_element()
    });
    let t = find(&ls, "err");
    let r = rows(&t);
    assert!(r.len() > 1 && r.len() <= 4, "{r:?}");
    for row in &r {
        assert!(row.trim().chars().count() >= 2, "a column of letters: {r:?}");
    }
    assert!(gpui::text_probe::take().iter().all(|p| !p.broke_a_word));
    assert_inside(&t, "error");
}

/// A content-sized column takes the widest word of its wrapping text when squeezed, and the whole
/// line when it has room (max-content), never 0.
#[gpui::test]
fn content_sized_column_is_as_wide_as_the_text_it_can_have(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(600.0)).flex().flex_row())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .child(text(ls, "t", "uno dos tres")),
            )
            .into_any_element()
    });
    let t = find(&ls, "t");
    assert_eq!(rows(&t), vec!["uno dos tres".to_string()]);
    assert!((f32::from(t.bounds().size.width) - 12.0 * CH).abs() <= 1.0);
}

/// Several wrapping texts beside each other share the row by their words: nobody breaks a word
/// while another has slack, and nobody is a column.
#[gpui::test]
fn siblings_squeezed_in_a_row_wrap_between_words(cx: &mut gpui::TestAppContext) {
    gpui::text_probe::start();
    let ls = run(cx, |ls| {
        small(div().w(px(150.0)).flex().flex_row().gap(px(10.0)))
            .child(div().child(text(ls, "a", "Modo claro automatico del sistema")))
            .child(div().child(text(ls, "b", "Sigue a Windows siempre")))
            .into_any_element()
    });
    for name in ["a", "b"] {
        let t = find(&ls, name);
        assert!(
            f32::from(t.bounds().size.width) >= 9.0 * CH - 1.0,
            "{name}: {:?}",
            t.bounds()
        );
        assert_inside(&t, name);
    }
    assert!(gpui::text_probe::take().iter().all(|p| !p.broke_a_word));
}

/// A note and a button in a wrapping row (settings "Más opciones de layout"): the row wraps the
/// button under the note; the note keeps the whole width, it is not squeezed to its widest word.
#[gpui::test]
fn note_in_a_wrapping_row_keeps_its_width_and_the_action_wraps(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(
            div()
                .w(px(500.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(12.0)),
        )
        .child(text(
            ls,
            "note",
            "El preset, las pestañas, la barra lateral, el panel derecho y las divisiones",
        ))
        .child(div().flex().flex_none().w(px(120.0)).h(px(20.0)))
        .into_any_element()
    });
    let n = find(&ls, "note");
    // 76 chars * 6 = 456 px: fits the 500 px row alone, the 120 px block goes to the next line
    assert!(
        f32::from(n.bounds().size.width) >= 400.0,
        "note squeezed: {:?} {:?}",
        n.bounds(),
        rows(&n)
    );
    assert_eq!(rows(&n).len(), 1);
}

/// The same note, inside the settings page skeleton (a centered, `max_w`-capped column in a wider
/// view): the cap, not the view, is the row's width.
#[gpui::test]
fn note_in_a_wrapping_row_inside_a_capped_centered_page(cx: &mut gpui::TestAppContext) {
    let ls = run(cx, |ls| {
        small(div().w(px(1000.0)).flex().flex_col().items_center())
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(500.0))
                    .flex()
                    .flex_col()
                    .child(
                        div().w_full().min_w_0().flex().flex_col().child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .items_center()
                                .gap(px(12.0))
                                .child(div().px(px(4.0)).child(text(
                                    ls,
                                    "note",
                                    "El preset, las pestañas, la barra lateral, el panel derecho y las divisiones",
                                )))
                                .child(div().flex().flex_none().w(px(120.0)).h(px(20.0))),
                        ),
                    ),
            )
            .into_any_element()
    });
    let n = find(&ls, "note");
    assert!(
        f32::from(n.bounds().size.width) >= 400.0,
        "note squeezed: {:?} {:?}",
        n.bounds(),
        rows(&n)
    );
}
