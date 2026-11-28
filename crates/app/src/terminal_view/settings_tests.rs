//! The `[terminal]` and `[appearance]` settings that act on terminals: each test proves that the
//! behaviour changes with the setting (a profile runs, the history is as long as asked, the cursor
//! takes its shape, a selection is copied, Ctrl+V is sent or pasted, the grid follows the font).
//!
//! The windows are gpui test windows; the shells are real ConPTY sessions of `cmd.exe`. Nothing is
//! sent to the desktop.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::{ClipboardItem, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use ui_kit::testing;

use super::*;

/// `SELACHI_SHELL` overrides every profile (a developer switch): tests of profiles need it unset.
fn shell_override_set() -> bool {
    std::env::var_os("SELACHI_SHELL").is_some()
}

fn auto() -> (String, Vec<String>) {
    (
        "pwsh.exe".to_string(),
        vec!["-NoLogo".to_string(), "-Integration".to_string()],
    )
}

fn profile(name: &str, program: &str) -> Profile {
    Profile {
        name: name.to_string(),
        program: program.to_string(),
        ..Profile::default()
    }
}

fn settings(default_profile: &str, profiles: Vec<Profile>) -> TermSettings {
    TermSettings {
        default_profile: default_profile.to_string(),
        profiles,
        ..TermSettings::default()
    }
}

/// A profile that starts `cmd.exe /c echo <text>` (so the output is a known line on screen).
fn echo_profile(text: &str) -> Profile {
    Profile {
        args: vec!["/c".into(), "echo".into(), text.into()],
        ..profile("echo", "cmd.exe")
    }
}

fn exe() -> String {
    std::env::current_exe().unwrap().display().to_string()
}

// ---- shell and profiles ------------------------------------------------------------------

#[test]
fn a_named_profile_is_the_shell_of_new_tabs() {
    let mut dev = profile("Dev", &exe());
    dev.args = vec!["--flag".into()];
    dev.cwd = Some(r"C:\work".into());
    dev.env = BTreeMap::from([("A".to_string(), "1".to_string())]);
    // the name matches without regard to case
    let plan = resolve_shell(&settings("dev", vec![dev]), None, |_| true, auto);
    assert_eq!(plan.program, exe());
    assert_eq!(plan.args, ["--flag"]);
    assert_eq!(plan.cwd, Some(PathBuf::from(r"C:\work")));
    assert_eq!(plan.env, [("A".to_string(), "1".to_string())]);
    assert_eq!(plan.hint, None);
    assert_eq!(
        plan.fallback,
        Some(auto()),
        "a broken program falls back to this"
    );
}

#[test]
fn automatic_and_unknown_names_use_the_default_shell() {
    let profiles = vec![profile("Dev", "cmd.exe")];
    for wanted in ["", "  ", "Does not exist"] {
        let plan = resolve_shell(&settings(wanted, profiles.clone()), None, |_| true, auto);
        assert_eq!((plan.program.as_str(), plan.args), ("pwsh.exe", auto().1));
        assert_eq!(plan.hint, None);
        assert!(plan.env.is_empty() && plan.cwd.is_none() && plan.fallback.is_none());
    }
}

#[test]
fn a_profile_whose_program_is_missing_falls_back_with_a_hint() {
    let s = settings("Ghost", vec![profile("Ghost", "selachi-ghost.exe")]);
    let plan = resolve_shell(&s, None, |_| false, auto);
    assert_eq!(plan.program, "pwsh.exe", "never a dead tab");
    let hint = plan.hint.expect("the user is told");
    assert!(
        hint.contains("Ghost") && hint.contains("selachi-ghost.exe") && hint.contains("pwsh"),
        "{hint}"
    );
}

#[test]
fn selachi_shell_overrides_the_profile() {
    let s = settings("Dev", vec![profile("Dev", "cmd.exe")]);
    let plan = resolve_shell(&s, Some("powershell.exe".into()), |_| true, auto);
    assert_eq!(plan.program, "powershell.exe");
    assert_eq!(plan.args, auto().1, "the shell integration stays");
    assert!(plan.fallback.is_none() && plan.hint.is_none());
}

#[test]
fn a_powershell_profile_without_arguments_keeps_the_shell_integration() {
    let s = settings("PS", vec![profile("PS", "pwsh.exe")]);
    assert_eq!(resolve_shell(&s, None, |_| true, auto).args, auto().1);
    let mut explicit = profile("PS", r"C:\Program Files\PowerShell\7\pwsh.exe");
    explicit.args = vec!["-NoProfile".into()];
    let s = settings("PS", vec![explicit]);
    assert_eq!(resolve_shell(&s, None, |_| true, auto).args, ["-NoProfile"]);
    // other shells get exactly what the profile says
    let s = settings("c", vec![profile("c", "cmd.exe")]);
    assert!(resolve_shell(&s, None, |_| true, auto).args.is_empty());
}

#[gpui::test]
fn a_new_tab_opens_the_default_profile_in_its_folder_with_its_scrollback(cx: &mut TestAppContext) {
    if shell_override_set() {
        return;
    }
    let dir = std::env::temp_dir();
    let mut p = echo_profile("x");
    p.cwd = Some(dir.display().to_string());
    let mut s = settings("echo", vec![p]);
    s.scrollback = 2_500;
    let built = build_deferred(cx, s, None);
    let cfg = built.cfg;
    assert_eq!(cfg.program, "cmd.exe");
    assert_eq!(cfg.args, ["/c", "echo", "x"]);
    assert_eq!(cfg.cwd.as_deref(), Some(dir.as_path()));
    assert_eq!(cfg.scrollback, 2_500, "the history length of the setting");
    assert_eq!(built.shell_name.as_ref(), "cmd");
    assert!(built.hint.is_none());
}

#[gpui::test]
fn the_folder_a_tab_was_opened_in_wins_over_the_profile_folder(cx: &mut TestAppContext) {
    if shell_override_set() {
        return;
    }
    let mut p = echo_profile("x");
    p.cwd = Some(std::env::temp_dir().display().to_string());
    let mine = PathBuf::from(r"C:\Windows");
    let built = build_deferred(cx, settings("echo", vec![p]), Some(mine.clone()));
    assert_eq!(built.cfg.cwd.as_deref(), Some(mine.as_path()));
}

#[gpui::test]
fn a_missing_profile_program_opens_the_automatic_shell_and_says_so(cx: &mut TestAppContext) {
    if shell_override_set() {
        return;
    }
    let s = settings("Ghost", vec![profile("Ghost", "selachi-ghost-shell.exe")]);
    let built = build_deferred(cx, s, None);
    let program = built.cfg.program;
    assert_ne!(program, "selachi-ghost-shell.exe");
    assert!(
        program.contains("pwsh") || program.contains("powershell"),
        "{program}"
    );
    let hint = built.hint.expect("visible hint");
    assert!(hint.contains("Ghost") && hint.contains("selachi-ghost-shell.exe"));
}

#[gpui::test]
fn the_profile_program_arguments_and_environment_really_run(cx: &mut TestAppContext) {
    if shell_override_set() {
        return;
    }
    let mut p = profile("env", "cmd.exe");
    p.args = vec!["/k".into(), "echo started-by-profile".into()];
    p.env = BTreeMap::from([("SELACHI_T".to_string(), "from-profile-env".to_string())]);
    let (view, vcx) = open(cx, settings("env", vec![p]), false);
    let text = wait_for(vcx, &view, |t| t.contains("started-by-profile"));
    assert!(
        text.contains("started-by-profile"),
        "the profile arguments ran:\n{text}"
    );
    vcx.update(|_, cx| view.update(cx, |v, cx| v.send_text("echo %SELACHI_T%", true, cx)));
    let text = wait_for(vcx, &view, |t| {
        t.lines().any(|l| l.trim() == "from-profile-env")
    });
    assert!(
        text.lines().any(|l| l.trim() == "from-profile-env"),
        "the profile env ran:\n{text}"
    );
}

#[gpui::test]
fn a_profile_that_exists_but_does_not_start_falls_back(cx: &mut TestAppContext) {
    if shell_override_set() {
        return;
    }
    // A file that is there but is not a program: `program_exists` accepts it, spawning fails.
    let junk =
        std::env::temp_dir().join(format!("selachi-not-a-program-{}.exe", std::process::id()));
    std::fs::write(&junk, b"not an executable").unwrap();
    let s = settings("Junk", vec![profile("Junk", &junk.display().to_string())]);
    let (view, vcx) = open(cx, s, false);
    // the shell that replaces it starts on a worker thread: wait for the hint
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut hint = None;
    while Instant::now() < deadline && hint.is_none() {
        vcx.run_until_parked();
        hint = vcx.update(|_, cx| view.read(cx).hint.clone());
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_file(&junk).ok();
    let hint = hint.expect("the failure is explained");
    assert!(
        hint.contains("Junk") || hint.contains("selachi-not-a-program"),
        "{hint}"
    );
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(v.spawn_error.is_none(), "not a dead tab");
        assert!(
            v.shell_name().contains("pwsh") || v.shell_name().contains("powershell"),
            "{}",
            v.shell_name()
        );
    });
}

// ---- scrollback --------------------------------------------------------------------------

#[test]
fn scrollback_is_clamped_to_what_the_config_accepts() {
    let lines = |n: u32| {
        TermSettings {
            scrollback: n,
            ..TermSettings::default()
        }
        .scrollback_lines()
    };
    assert_eq!(lines(0), 0);
    assert_eq!(lines(2_500), 2_500);
    assert_eq!(lines(u32::MAX), MAX_SCROLLBACK);
    // the store accepts exactly up to the same limit
    let mut cfg = Config::default();
    cfg.terminal.scrollback = MAX_SCROLLBACK as u32;
    assert!(cfg.validate().is_empty());
    cfg.terminal.scrollback += 1;
    assert!(!cfg.validate().is_empty());
}

// ---- cursor ------------------------------------------------------------------------------

#[test]
fn cursor_shapes() {
    use crate::terminal_element::cursor_shape;
    let cell = Bounds::new(
        gpui::point(px(10.0), px(20.0)),
        gpui::size(px(8.0), px(17.0)),
    );
    assert_eq!(cursor_shape(CursorStyle::Block, cell), cell);
    let bar = cursor_shape(CursorStyle::Bar, cell);
    assert_eq!(
        (bar.origin, bar.size.width, bar.size.height),
        (cell.origin, px(2.0), px(17.0))
    );
    let under = cursor_shape(CursorStyle::Underline, cell);
    assert_eq!(
        (
            under.origin.x,
            under.origin.y,
            under.size.width,
            under.size.height
        ),
        (px(10.0), px(35.0), px(8.0), px(2.0))
    );
}

#[gpui::test]
fn the_cursor_of_an_open_terminal_follows_the_setting_live(cx: &mut TestAppContext) {
    let (view, vcx) = open(cx, plain_cmd(), false);
    wait_for(vcx, &view, |t| t.contains('>'));
    draw(vcx);
    let (cell, shape) = cursor_of(vcx, &view);
    assert_eq!(shape, cell, "block: the whole cell");
    for (style, width, height) in [
        (CursorStyle::Bar, px(2.0), cell.size.height),
        (CursorStyle::Underline, cell.size.width, px(2.0)),
        (CursorStyle::Block, cell.size.width, cell.size.height),
    ] {
        set_settings(vcx, &view, |s| s.cursor_style = style);
        draw(vcx);
        let (_, shape) = cursor_of(vcx, &view);
        assert_eq!(
            (shape.size.width, shape.size.height),
            (width, height),
            "{style:?}"
        );
    }
}

// ---- selection, copy on select, Ctrl+V ---------------------------------------------------

#[gpui::test]
fn copy_on_select_copies_when_the_button_goes_up(cx: &mut TestAppContext) {
    let (view, vcx) = open(cx, plain_cmd(), false);
    wait_for(vcx, &view, |t| t.contains('>'));
    vcx.update(|_, cx| view.update(cx, |v, cx| v.send_text("echo selachi-pick", true, cx)));
    wait_for(vcx, &view, |t| {
        t.lines().any(|l| l.trim() == "selachi-pick")
    });
    draw(vcx);
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("before".into())));

    // off: the text is selected (and highlighted) but the clipboard is left alone
    drag_over(vcx, &view, "selachi-pick", 0, 7);
    assert_eq!(selected(vcx, &view).as_deref(), Some("selachi"));
    assert_eq!(clipboard(vcx).as_deref(), Some("before"));
    // Ctrl+Shift+C copies on demand
    testing::press(vcx, "ctrl-shift-c");
    assert_eq!(clipboard(vcx).as_deref(), Some("selachi"));

    // on (live, no new tab): the same drag copies on mouse-up
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("before".into())));
    set_settings(vcx, &view, |s| s.copy_on_select = true);
    drag_over(vcx, &view, "selachi-pick", 8, 12);
    assert_eq!(clipboard(vcx).as_deref(), Some("pick"));

    // a click without a drag selects nothing and copies nothing
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("keep".into())));
    drag_over(vcx, &view, "selachi-pick", 3, 3);
    assert_eq!(selected(vcx, &view), None);
    assert_eq!(clipboard(vcx).as_deref(), Some("keep"));
}

#[gpui::test]
fn ctrl_v_pastes_only_when_the_setting_is_on_and_ctrl_shift_v_always(cx: &mut TestAppContext) {
    // on (the default): Ctrl+V pastes
    let (view, vcx) = open(cx, plain_cmd(), false);
    wait_for(vcx, &view, |t| t.contains('>'));
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("QQQ".into())));
    testing::press(vcx, "ctrl-v");
    let text = wait_for(vcx, &view, |t| t.contains("QQQ"));
    assert!(text.contains("QQQ"), "Ctrl+V did not paste:\n{text}");

    // off: Ctrl+V goes to the program (^V, 0x16) and pastes nothing; Ctrl+Shift+V still pastes
    set_settings(vcx, &view, |s| s.paste_ctrl_v = false);
    vcx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("ZZZ".into())));
    testing::press(vcx, "ctrl-v");
    std::thread::sleep(Duration::from_millis(300));
    vcx.run_until_parked();
    assert!(
        !screen(vcx, &view).contains("ZZZ"),
        "Ctrl+V pasted while it is off"
    );
    testing::press(vcx, "ctrl-shift-v");
    let text = wait_for(vcx, &view, |t| t.contains("ZZZ"));
    assert!(text.contains("ZZZ") && !text.contains("ZZZZZZ"), "{text}");
}

#[test]
fn ctrl_v_is_the_control_byte_16() {
    let key = term_core::KeyInput {
        key: "v",
        key_char: Some("v"),
        ctrl: true,
        alt: false,
        shift: false,
    };
    assert_eq!(encode_key(&key, false), Some(vec![0x16]));
}

// ---- typography --------------------------------------------------------------------------

#[gpui::test]
fn the_grid_follows_the_font_size_and_leading_live(cx: &mut TestAppContext) {
    let (view, vcx) = open(cx, plain_cmd(), false);
    wait_for(vcx, &view, |t| t.contains('>'));
    draw(vcx);
    let (cols0, rows0, cw0, ch0) = grid_of(vcx, &view);
    let theme = Theme::dark();
    let bigger = Theme {
        typography: theme
            .typography
            .with_fonts("DM Sans", "Geist Mono", 26.0, 1.6),
        ..theme
    };
    vcx.update(|_, cx| view.update(cx, |v, cx| v.set_theme(bigger, cx)));
    draw(vcx);
    let (cols1, rows1, cw1, ch1) = grid_of(vcx, &view);
    assert!(
        cw1 > cw0 * 1.8 && ch1 > ch0 * 2.0,
        "cell {cw0}x{ch0} -> {cw1}x{ch1}"
    );
    assert!(
        cols1 < cols0 && rows1 < rows0,
        "grid {cols0}x{rows0} -> {cols1}x{rows1}"
    );
    // the PTY grid is exactly what fits the new cell
    let (w, h) = vcx.update(|window, _| {
        let vp = window.viewport_size();
        (f32::from(vp.width), f32::from(vp.height))
    });
    let fit = |len: f32, cell: f32| ((len - 2.0 * TERM_PAD) / cell).floor() as usize;
    assert_eq!(cols1, fit(w, cw1));
    assert_eq!(rows1, fit(h, ch1));
    // and back
    vcx.update(|_, cx| view.update(cx, |v, cx| v.set_theme(theme, cx)));
    draw(vcx);
    assert_eq!(grid_of(vcx, &view), (cols0, rows0, cw0, ch0));
}

// ---- helpers -----------------------------------------------------------------------------

fn plain_cmd() -> TermSettings {
    settings("cmd", vec![profile("cmd", "cmd.exe")])
}

/// The terminal fills a column, like the card of the shell.
struct Host(Entity<TerminalView>);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().flex_col().child(self.0.clone())
    }
}

fn open(
    cx: &mut TestAppContext,
    settings: TermSettings,
    deferred: bool,
) -> (Entity<TerminalView>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(settings));
    let (host, vcx) = testing::open_window(cx, move |window, cx| {
        Host(cx.new(|cx| {
            if deferred {
                TerminalView::new_deferred(Theme::dark(), None, window, cx)
            } else {
                TerminalView::new(Theme::dark(), None, window, cx)
            }
        }))
    });
    let view = vcx.update(|_, cx| host.read(cx).0.clone());
    (view, vcx)
}

/// What a deferred view was built to run (read before its first paint spawns the shell).
struct Built {
    cfg: SessionConfig,
    hint: Option<SharedString>,
    shell_name: SharedString,
}

fn build_deferred(cx: &mut TestAppContext, settings: TermSettings, cwd: Option<PathBuf>) -> Built {
    cx.update(|cx| cx.set_global(settings));
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = out.clone();
    let _ = testing::open_window(cx, move |window, cx| {
        let view = cx.new(|cx| TerminalView::new_deferred(Theme::dark(), cwd, window, cx));
        let v = view.read(cx);
        *sink.borrow_mut() = Some(Built {
            cfg: v.pending.clone().expect("deferred"),
            hint: v.hint.clone(),
            shell_name: v.shell_name().clone(),
        });
        Host(view)
    });
    out.take().expect("built")
}

/// Changes the live settings like the shell does on a config reload: the global, then a repaint.
fn set_settings(
    vcx: &mut VisualTestContext,
    view: &Entity<TerminalView>,
    f: impl FnOnce(&mut TermSettings),
) {
    vcx.update(|_, cx| {
        let mut s = TermSettings::current(cx);
        f(&mut s);
        cx.set_global(s);
        view.update(cx, |_, cx| cx.notify());
    });
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
}

fn screen(vcx: &mut VisualTestContext, view: &Entity<TerminalView>) -> String {
    vcx.update(|_, cx| {
        view.read(cx)
            .session
            .as_ref()
            .map(|s| s.snapshot().text())
            .unwrap_or_default()
    })
}

/// Pumps the app until the screen satisfies `done` (at most 15 s) and returns the screen.
fn wait_for(
    vcx: &mut VisualTestContext,
    view: &Entity<TerminalView>,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        vcx.run_until_parked();
        let text = screen(vcx, view);
        if done(&text) || Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cursor_of(
    vcx: &mut VisualTestContext,
    view: &Entity<TerminalView>,
) -> (Bounds<Pixels>, Bounds<Pixels>) {
    vcx.update(|_, cx| {
        let v = view.read(cx);
        (
            v.cursor_bounds.get().expect("a cursor cell"),
            v.cursor_shape.get().expect("a painted cursor"),
        )
    })
}

/// `(cols, rows, cell width, cell height)` of the PTY grid and the cell the view paints with.
fn grid_of(vcx: &mut VisualTestContext, view: &Entity<TerminalView>) -> (usize, usize, f32, f32) {
    vcx.update(|window, cx| {
        let v = view.read(cx);
        let snap = v.session.as_ref().unwrap().snapshot();
        let m = v.metrics(window);
        (
            snap.cols,
            snap.rows,
            f32::from(m.width),
            f32::from(m.height),
        )
    })
}

fn selected(vcx: &mut VisualTestContext, view: &Entity<TerminalView>) -> Option<String> {
    vcx.update(|_, cx| view.read(cx).selection_text())
}

fn clipboard(vcx: &mut VisualTestContext) -> Option<String> {
    vcx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// Presses the left button on column `from` of the screen line that contains `line`, drags to
/// column `to` (inclusive) and lets go: real mouse events through the window.
fn drag_over(
    vcx: &mut VisualTestContext,
    view: &Entity<TerminalView>,
    line: &str,
    from: usize,
    to: usize,
) {
    let (row, base, origin, cw, ch) = vcx.update(|_, cx| {
        let v = view.read(cx);
        let snap = v.session.as_ref().unwrap().snapshot();
        // the line of the output, not the command that printed it
        let row = (0..snap.rows)
            .find(|r| snap.line_text(*r).trim() == line)
            .expect("the line is on screen");
        let base = snap.line_text(row).find(line).unwrap();
        let (cw, ch) = v.block_ui.cell.get();
        (row, base, v.block_ui.origin.get(), cw, ch)
    });
    let at = |col: usize, frac: f32| {
        gpui::point(
            origin.x + px(cw * (col as f32 + frac)),
            origin.y + px(ch * (row as f32 + 0.5)),
        )
    };
    let none = Modifiers::none();
    vcx.simulate_mouse_down(at(base + from, 0.25), MouseButton::Left, none);
    // the last cell is included when the pointer is on its right half
    let end = at(base + to.saturating_sub(1).max(from), 0.75);
    let end = if to == from {
        at(base + from, 0.25)
    } else {
        end
    };
    vcx.simulate_mouse_move(end, MouseButton::Left, none);
    vcx.simulate_mouse_up(end, MouseButton::Left, none);
    vcx.run_until_parked();
}
