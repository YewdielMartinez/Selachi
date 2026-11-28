//! The layout contract of a settings screen, as checks over what the components reported
//! (`ui_kit::layout_probe`) and what gpui painted (`gpui::text_probe`):
//!
//! * (a) every control stays inside its card;
//! * (b) the controls of a group that sit beside their text share one right edge (+-1 px);
//! * (c) no text box is a column: narrower than 24 px with more than 3 rows;
//! * a wrapped text never paints wider than its box or breaks a word, a field never collapses,
//!   a pill is one row, and a text column never runs under its own control.
//!
//! Shared by the settings view tests (`layout_tests`, a demo window with a test font) and by the
//! window test of the real `AppShell` (the `selachi` crate, with the Settings tab open), so both
//! prove the same thing.

use gpui::{Bounds, Pixels, VisualTestContext, size, px};
use ui_kit::layout_probe::{self, BoxKind, BoxProbe};

const EDGE_TOLERANCE: f32 = 1.0;

/// Records a problem instead of stopping at the first one.
macro_rules! ensure {
    ($bad:expr, $cond:expr, $($arg:tt)+) => {
        if $cond {
        } else {
            $bad.push(format!($($arg)+));
        }
    };
}

fn f(p: Pixels) -> f32 {
    f32::from(p)
}

/// Lays the screen out at `width` (tall window) and returns the boxes and texts of the last frame.
pub fn measure(
    vcx: &mut VisualTestContext,
    width: f32,
) -> (Vec<BoxProbe>, Vec<gpui::text_probe::TextProbe>) {
    vcx.simulate_resize(size(px(width), px(6000.0)));
    // The width probe of the view measures while painting and the shell hands its zone width down
    // on the next frames: frames are drawn until two in a row report the same boxes (a screen that
    // keeps moving after 16 frames is reported as it is).
    let mut last: Option<String> = None;
    let mut frames = 0;
    loop {
        layout_probe::start();
        gpui::text_probe::start();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        let (boxes, texts) = (layout_probe::take(), gpui::text_probe::take());
        frames += 1;
        let key = format!(
            "{:?}",
            boxes
                .iter()
                .map(|b| (b.kind as u8, b.label.clone(), b.bounds))
                .collect::<Vec<_>>()
        );
        if (last.as_deref() == Some(key.as_str()) && frames >= 3) || frames >= 16 {
            return (boxes, texts);
        }
        last = Some(key);
    }
}

fn contains(b: &Bounds<Pixels>, x: f32, y: f32) -> bool {
    x >= f(b.left()) - 0.5
        && x <= f(b.right()) + 0.5
        && y >= f(b.top()) - 0.5
        && y <= f(b.bottom()) + 0.5
}

fn area(b: &Bounds<Pixels>) -> f32 {
    f(b.size.width) * f(b.size.height)
}

/// The smallest card around the point `(x, y)`.
fn card_at(boxes: &[BoxProbe], x: f32, y: f32) -> Option<&BoxProbe> {
    boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Card && contains(&b.bounds, x, y))
        .min_by(|a, b| area(&a.bounds).total_cmp(&area(&b.bounds)))
}

pub fn check(
    screen: &dyn std::fmt::Debug,
    width: f32,
    boxes: &[BoxProbe],
    texts: &[gpui::text_probe::TextProbe],
    bad: &mut Vec<String>,
) {
    let at = |what: &str| format!("{screen:?} at {width}px: {what}");
    ensure!(bad, !boxes.is_empty(), "{}", at("nothing reported its box"));
    // (a) controls inside their card; (b) one right edge per group.
    let mut edges: Vec<(String, String, f32)> = Vec::new();
    for c in boxes.iter().filter(|b| b.kind == BoxKind::Control) {
        let b = &c.bounds;
        let (cx, cy) = (f(b.left()) + 1.0, f(b.top()) + 1.0);
        let Some(card) = card_at(boxes, cx, cy) else {
            continue;
        };
        ensure!(
            bad,
            f(b.left()) >= f(card.bounds.left()) - EDGE_TOLERANCE
                && f(b.right()) <= f(card.bounds.right()) + EDGE_TOLERANCE,
            "{}",
            at(&format!(
                "control of `{}` [{}..{}] leaves card `{}` [{}..{}]",
                c.label,
                f(b.left()),
                f(b.right()),
                card.label,
                f(card.bounds.left()),
                f(card.bounds.right())
            ))
        );
        // A control that wrapped under its text counts as stacked (its left edge is the text's).
        let text = boxes.iter().find(|t| {
            t.kind == BoxKind::Text
                && t.label == c.label
                && f(t.bounds.top()) <= f(b.bottom())
                && f(t.bounds.bottom()) >= f(b.top()) - 40.0
                && (f(t.bounds.top()) - f(b.top())).abs() < 80.0
        });
        let wrapped = text.is_some_and(|t| f(b.top()) >= f(t.bounds.bottom()) - 1.0);
        if !c.stacked && !wrapped {
            edges.push((card.label.clone(), c.label.clone(), f(b.right())));
        }
    }
    let mut cards: Vec<&str> = edges.iter().map(|(c, _, _)| c.as_str()).collect();
    cards.sort_unstable();
    cards.dedup();
    for card in cards {
        let group: Vec<_> = edges.iter().filter(|(c, _, _)| c == card).collect();
        let first = group[0].2;
        for (_, label, right) in &group {
            ensure!(
                bad,
                (right - first).abs() <= EDGE_TOLERANCE,
                "{}",
                at(&format!(
                    "controls in card `{card}` do not share a right edge: `{}` ends at {first}, `{label}` at {right}",
                    group[0].1
                ))
            );
        }
    }
    // A text column never runs under its own control.
    for row in boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Row && !b.stacked)
    {
        // (a wrapped control is under the text: nothing to compare)
        let inside = |k: BoxKind| {
            boxes.iter().find(|b| {
                b.kind == k
                    && b.label == row.label
                    && contains(
                        &row.bounds,
                        f(b.bounds.left()) + 0.5,
                        f(b.bounds.top()) + 0.5,
                    )
            })
        };
        if let (Some(t), Some(c)) = (inside(BoxKind::Text), inside(BoxKind::Control)) {
            if f(c.bounds.top()) >= f(t.bounds.bottom()) - 1.0 {
                continue;
            }
            ensure!(
                bad,
                f(t.bounds.right()) <= f(c.bounds.left()) + EDGE_TOLERANCE,
                "{}",
                at(&format!(
                    "text of `{}` ends at {} but its control starts at {}",
                    row.label,
                    f(t.bounds.right()),
                    f(c.bounds.left())
                ))
            );
        }
    }
    // Fields and drop-downs never collapse.
    for b in boxes
        .iter()
        .filter(|b| matches!(b.kind, BoxKind::Field | BoxKind::Select))
    {
        ensure!(
            bad,
            f(b.bounds.size.width) >= 48.0 && f(b.bounds.size.height) >= 20.0,
            "{}",
            at(&format!("{:?} collapsed to {:?}", b.kind, b.bounds.size))
        );
    }
    // (c) no column of text; a wrapped text stays inside its box; nothing paints out of its card.
    for t in texts {
        ensure!(
            bad,
            !(f(t.bounds.size.width) < 24.0 && t.rows > 3),
            "{}",
            at(&format!(
                "text `{}` is a {} px wide column of {} rows",
                t.text.replace('\n', "|"),
                f(t.bounds.size.width),
                t.rows
            ))
        );
        ensure!(
            bad,
            !t.broke_a_word,
            "{}",
            at(&format!(
                "text `{}` is broken in the middle of a word",
                t.text.replace(char::from(10), "|")
            ))
        );
        if t.rows > 1 {
            ensure!(
                bad,
                f(t.painted_width) <= f(t.bounds.size.width) + 1.0,
                "{}",
                at(&format!(
                    "wrapped text `{}` paints {} px in a {} px box",
                    t.text.replace('\n', "|"),
                    f(t.painted_width),
                    f(t.bounds.size.width)
                ))
            );
        }
        // a group title (the label above a card) is one row
        ensure!(
            bad,
            !(t.rows > 1
                && boxes
                    .iter()
                    .any(|b| b.kind == BoxKind::Card && !b.label.is_empty() && b.label == t.text)),
            "{}",
            at(&format!("group title `{}` wraps into {} rows", t.text, t.rows))
        );
        // a sentence squeezed into a column of one or two words per row
        ensure!(
            bad,
            !(t.rows >= 4 && f(t.painted_width) < 100.0 && t.text.chars().count() >= 20),
            "{}",
            at(&format!(
                "text `{}` is a column of words: {} rows, {} px wide",
                t.text.replace(char::from(10), "|"),
                t.rows,
                f(t.painted_width)
            ))
        );
        // a text of one word (a pill, a badge, a value) is one row: it is never stacked
        ensure!(
            bad,
            !(t.rows > 1 && !t.text.trim().contains(char::is_whitespace)),
            "{}",
            at(&format!(
                "one-word text `{}` is stacked in {} rows ({} px wide box)",
                t.text.replace(char::from(10), "|"),
                t.rows,
                f(t.bounds.size.width)
            ))
        );
        let (x, y) = (f(t.bounds.left()) + 1.0, f(t.bounds.top()) + 1.0);
        if let Some(card) = card_at(boxes, x, y) {
            let right = f(t.bounds.left()) + f(t.bounds.size.width).max(f(t.painted_width));
            ensure!(
                bad,
                right <= f(card.bounds.right()) + EDGE_TOLERANCE,
                "{}",
                at(&format!(
                    "text `{}` reaches {right}, past card `{}` ending at {}",
                    t.text.replace('\n', "|"),
                    card.label,
                    f(card.bounds.right())
                ))
            );
        }
    }
}

