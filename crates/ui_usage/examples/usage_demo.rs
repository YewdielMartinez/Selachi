//! The usage panel over a synthetic database (screenshots) or a read-only copy of the real one.
//!
//! ```text
//! cargo run -p ui_usage --example usage_demo --release -- [dark|light] [--zone right|sidebar|main|bottom]
//!     [--size WxH] [--range session|today|week|7d] [--no-project] [--real]
//! ```
//!
//! * Default: a temp database with invented events for Claude, Codex and OpenCode across 14 days,
//!   four branches, and rate-limit snapshots (Claude fresh, Codex 5 h stale, Codex 7 days fresh).
//!   Nothing private is involved: use this one for screenshots.
//! * `--real`: copies `%APPDATA%\Selachi\selachi.db` (+ `-wal` / `-shm`) into a temp folder and
//!   reads the copy, so the real database is never opened for writing. The numbers are the user's
//!   own: never publish a screenshot taken with it.

use std::path::{Path, PathBuf};

use agents_svc::{Agent, RateLimitSnapshot, Store, TokenCounts, UsageEvent, UsageExtras};
use gpui::{
    AnyView, App, Application, Bounds, Context, Entity, IntoElement, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use panel_api::{PanelCtx, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Theme};
use ui_kit::{Card, CardTone, Icon, Logo, Pill};
use ui_usage::{UsageModel, UsagePanelProvider, UsageRange};

const H: i64 = 3_600_000;
const MIN: i64 = 60_000;
const DEMO_ROOT: &str = "C:\\Users\\demo\\Selachi";

struct Host {
    theme: Theme,
    zone: PanelZone,
    panel: AnyView,
    panel_width: f32,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let c = t.colors;
        let panel = Card::new(&t).tone(CardTone::Raised, &t).overflow_hidden().h_full().child(self.panel.clone());
        let terminal = Card::new(&t)
            .tone(CardTone::Base, &t)
            .flex_1()
            .h_full()
            .p(px(16.0))
            .font_family(t.typography.mono_family)
            .text_size(px(t.typography.mono_size))
            .child("PS C:\\Users\\demo\\Selachi> ");
        let titlebar = div()
            .h(px(40.0))
            .flex_none()
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(Logo::new(t.mode).size(22.0))
            .child(Pill::new("t0", &t).icon(Icon::Terminal).label("selachi").active(true));
        let row = div().flex_1().min_h_0().px(px(8.0)).pb(px(8.0)).flex().flex_row().gap(px(8.0));
        let body = match self.zone {
            PanelZone::Sidebar => row.child(panel.flex_none().w(px(self.panel_width))).child(terminal),
            PanelZone::Main | PanelZone::Bottom => row.child(panel.flex_1()),
            PanelZone::Right => row.child(terminal).child(panel.flex_none().w(px(self.panel_width))),
        };
        div()
            .size_full()
            .bg(c.bg)
            .text_color(c.text)
            .font_family(t.typography.ui_family)
            .flex()
            .flex_col()
            .child(titlebar)
            .child(body)
    }
}

fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

// ----- synthetic data ----------------------------------------------------------------------------

/// Tiny deterministic generator (the demo must look the same on every run).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn between(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[allow(clippy::too_many_arguments)]
fn put(store: &Store, agent: Agent, model: &str, key: String, ts: i64, t: TokenCounts, branch: Option<&str>, cwd: &str) {
    let e = UsageEvent {
        agent,
        session_id: if agent == Agent::Claude && ts > now_ms() - 12 * H { "demo-session".into() } else { format!("s-{}", ts / (6 * H)) },
        ts_ms: ts,
        model: model.into(),
        tokens: t,
        cwd: Some(cwd.into()),
        git_branch: branch.map(str::to_string),
        msg_key: key,
    };
    let _ = if agent == Agent::OpenCode {
        let x = UsageExtras {
            reasoning: 0,
            reported_cost_usd: Some((t.input + t.output) as f64 / 1_000_000.0 * 3.0),
            cost_source: Some("opencode".into()),
            provider: Some("openrouter".into()),
        };
        store.upsert_usage_replace(&e, &x)
    } else {
        store.upsert_usage(&e)
    };
}

fn seed(path: &Path) -> anyhow::Result<()> {
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
    let store = Store::open(path)?;
    let now = now_ms();
    let mut rng = Lcg(42);
    let branches = ["main", "feat/usage-panel", "fix/pty-resize", "chore/deps"];

    // 14 days of activity: Claude most days, Codex some, OpenCode a few.
    for day in 0..14i64 {
        let weight = [3, 5, 2, 0, 0, 4, 6, 3, 5, 4, 2, 6, 5, 4][day as usize];
        for n in 0..weight {
            let ts = now - (13 - day) * 24 * H - (n as i64 + 1) * 90 * MIN - 8 * H;
            let branch = branches[rng.between(0, 3) as usize];
            let big = rng.between(1, 4);
            put(
                &store,
                Agent::Claude,
                if n % 3 == 0 { "claude-sonnet-5-5" } else { "claude-opus-5-5" },
                format!("c-{day}-{n}"),
                ts,
                TokenCounts { input: 40_000 * big, output: 90_000 * big, cache_create: 120_000 * big, cache_create_1h: 80_000 * big, cache_read: 4_000_000 * big },
                Some(branch),
                DEMO_ROOT,
            );
        }
        if day % 3 != 1 {
            let ts = now - (13 - day) * 24 * H - 10 * H;
            put(&store, Agent::Codex, "gpt-6.1-sol", format!("x-{day}"), ts, TokenCounts { input: 150_000 + day as u64 * 20_000, output: 60_000, cache_create: 0, cache_create_1h: 0, cache_read: 900_000 }, None, DEMO_ROOT);
        }
        if day % 5 == 2 {
            let ts = now - (13 - day) * 24 * H - 11 * H;
            put(&store, Agent::OpenCode, "deepseek-v4", format!("o-{day}"), ts, TokenCounts { input: 200_000, output: 80_000, cache_create: 0, cache_create_1h: 0, cache_read: 0 }, Some("main"), DEMO_ROOT);
        }
    }
    // Recent Claude activity (calibration before the snapshot, then a high current pace).
    for (i, (ago, big)) in [(165 * MIN, 4u64), (130 * MIN, 5), (95 * MIN, 4), (50 * MIN, 5), (30 * MIN, 5), (12 * MIN, 3), (5 * MIN, 2)].into_iter().enumerate() {
        put(
            &store,
            Agent::Claude,
            "claude-opus-5-5",
            format!("r-{i}"),
            now - ago,
            TokenCounts { input: 60_000 * big, output: 70_000 * big, cache_create: 40_000 * big, cache_create_1h: 20_000 * big, cache_read: 6_000_000 },
            Some("feat/usage-panel"),
            DEMO_ROOT,
        );
    }

    // Codex and OpenCode were used today as well.
    for (i, ago) in [100 * MIN, 35 * MIN].into_iter().enumerate() {
        put(&store, Agent::Codex, "gpt-6.1-sol", format!("xr-{i}"), now - ago, TokenCounts { input: 160_000, output: 70_000, cache_create: 0, cache_create_1h: 0, cache_read: 700_000 }, None, DEMO_ROOT);
    }
    put(&store, Agent::OpenCode, "deepseek-v4", "or-0".into(), now - 70 * MIN, TokenCounts { input: 250_000, output: 90_000, cache_create: 0, cache_create_1h: 0, cache_read: 0 }, Some("main"), DEMO_ROOT);

    let snap = |agent, window: &str, ago: i64, pct: f64, resets: i64, source: &str| RateLimitSnapshot {
        agent,
        ts_ms: now - ago,
        window: window.into(),
        used_percent: pct,
        resets_at_ms: Some(now + resets),
        source: source.into(),
    };
    for s in [
        snap(Agent::Claude, "five_hour", 2 * MIN, 62.0, 2 * H + 13 * MIN, "claude-statusline"),
        snap(Agent::Claude, "seven_day", 2 * MIN, 34.0, 3 * 24 * H + 5 * H, "claude-statusline"),
        // Codex: the 5 h window already reset (stale), the weekly one is recent.
        snap(Agent::Codex, "five_hour", 8 * H, 71.0, -3 * H, "codex-rollout"),
        snap(Agent::Codex, "seven_day", 50 * MIN, 21.0, 4 * 24 * H, "codex-rollout"),
    ] {
        store.insert_rate_limit(&s)?;
    }
    Ok(())
}

/// Copies the real database next to nothing else: the demo only ever opens the copy.
fn copy_real() -> anyhow::Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA").ok_or_else(|| anyhow::anyhow!("APPDATA is not set"))?;
    let src = PathBuf::from(appdata).join("Selachi").join("selachi.db");
    let dir = std::env::temp_dir().join("selachi-usage-demo-real");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let dst = dir.join("selachi.db");
    std::fs::copy(&src, &dst)?;
    for ext in ["-wal", "-shm"] {
        let (mut s, mut d) = (src.as_os_str().to_owned(), dst.as_os_str().to_owned());
        s.push(ext);
        d.push(ext);
        let _ = std::fs::copy(PathBuf::from(s), PathBuf::from(d));
    }
    Ok(dst)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|a| a == "light") { Mode::Light } else { Mode::Dark };
    let zone = match arg_value(&args, "--zone") {
        Some("sidebar") => PanelZone::Sidebar,
        Some("main") => PanelZone::Main,
        Some("bottom") => PanelZone::Bottom,
        _ => PanelZone::Right,
    };
    let panel_width = match zone {
        PanelZone::Sidebar => 264.0,
        PanelZone::Main | PanelZone::Bottom => 900.0,
        PanelZone::Right => 420.0,
    };
    let (w, h) = arg_value(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
        .unwrap_or(match zone {
            PanelZone::Main | PanelZone::Bottom => (1000.0, 820.0),
            _ => (900.0, 980.0),
        });
    let range = match arg_value(&args, "--range") {
        Some("session") => UsageRange::Session,
        Some("week") => UsageRange::Week,
        Some("7d") => UsageRange::Last7Days,
        _ => UsageRange::Today,
    };
    let real = args.iter().any(|a| a == "--real");
    let project_root: Option<PathBuf> = if args.iter().any(|a| a == "--no-project") {
        None
    } else if real {
        std::env::current_dir().ok()
    } else {
        Some(PathBuf::from(DEMO_ROOT))
    };

    let db = if real {
        copy_real()
    } else {
        let dir = std::env::temp_dir().join("selachi-usage-demo");
        std::fs::create_dir_all(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|()| {
                let p = dir.join("synthetic.db");
                seed(&p).map(|()| p)
            })
    };
    let db = match db {
        Ok(p) => p,
        Err(e) => {
            eprintln!("database: {e:#}");
            return;
        }
    };

    Application::new().with_assets(ui_kit::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::fonts::embedded()) {
            eprintln!("fonts: {e}");
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let theme = Theme::for_mode(mode);
        let model: Entity<UsageModel> = cx.new(|_| UsageModel::new());
        model.update(cx, |m, cx| {
            m.set_db_path(db, cx);
            m.set_session_id(Some("demo-session".into()), cx);
            m.set_range(range, cx);
        });
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let _ = cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let provider = UsagePanelProvider::new(model);
                let ctx = PanelCtx {
                    zone,
                    width_class: WidthClass::from_width(panel_width),
                    cwd: project_root.clone(),
                    project_root,
                    theme,
                };
                let panel = provider.build(ctx, window, cx);
                cx.new(|_| Host { theme, zone, panel, panel_width })
            },
        );
        cx.activate(true);
    });
}
