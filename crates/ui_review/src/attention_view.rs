//! `AttentionView`: the compact list of what needs you (agent mark, where, why, how long), usable
//! as a popover from the status bar or as a Sidebar / Right panel ([`AttentionPanelProvider`], id
//! `attention`).
//!
//! Zero hidden cost: the elapsed text is refreshed by a tick that only runs while the view is
//! being painted. The tick re-arms itself only if the view was rendered since the previous tick,
//! so a hidden view stops its timer (at most one more wake-up after it disappears) and a visible
//! one ticks every second while some item is under a minute old, every 15 s afterwards.

use std::time::Duration;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, Hsla, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use panel_api::{PanelCtx, PanelId, PanelProvider, PanelZone, WidthClass};
use theme::{Mode, Radius, Theme, rgb};
use ui_kit::{AgentMark, Icon, IconButton, SectionLabel, agent_icon, icon};

use crate::attention::{AttentionItem, AttentionKind, AttentionModel, format_wait};

/// Panel id (what `layout.right_panel` stores).
pub const PANEL_ID: &str = "attention";

/// Tick while some item is younger than a minute (the text counts seconds).
const FAST_TICK: Duration = Duration::from_secs(1);
/// Tick otherwise (the text counts minutes).
const SLOW_TICK: Duration = Duration::from_secs(15);

/// The list.
pub struct AttentionView {
    model: Entity<AttentionModel>,
    theme: Theme,
    width: WidthClass,
    header: bool,
    /// Painted since the last tick.
    rendered: bool,
    ticking: bool,
    task: Option<Task<()>>,
    ticks: usize,
    _observe: Subscription,
}

fn ink(theme: &Theme, dark: usize, light: usize) -> Hsla {
    rgb(theme.terminal.ansi[if theme.mode == Mode::Light { light } else { dark }]).into()
}

impl AttentionView {
    pub fn new(model: Entity<AttentionModel>, theme: Theme, width: WidthClass, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        Self { model, theme, width, header: true, rendered: false, ticking: false, task: None, ticks: 0, _observe: observe }
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn set_width_class(&mut self, width: WidthClass, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
            cx.notify();
        }
    }

    /// Shows or hides the "Atención" title (a popover usually brings its own).
    pub fn set_header(&mut self, header: bool, cx: &mut Context<Self>) {
        self.header = header;
        cx.notify();
    }

    /// Whether the elapsed-time timer is armed.
    pub fn tick_active(&self) -> bool {
        self.ticking
    }

    /// Ticks that refreshed the view so far.
    pub fn ticks(&self) -> usize {
        self.ticks
    }

    fn interval(&self, cx: &App) -> Duration {
        let youngest = self.model.read(cx).items().iter().map(|i| i.since.elapsed()).min();
        match youngest {
            Some(age) if age < Duration::from_secs(60) => FAST_TICK,
            _ => SLOW_TICK,
        }
    }

    /// Arms the tick if it is not running and there is something to count.
    fn ensure_ticking(&mut self, cx: &mut Context<Self>) {
        if self.ticking || self.model.read(cx).is_empty() {
            return;
        }
        self.ticking = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |v, cx| v.interval(cx)) else { break };
                cx.background_executor().timer(wait).await;
                match this.update(cx, |v, cx| v.on_tick(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        }));
    }

    /// One tick: refresh if the view was painted since the last one, otherwise stop for good.
    fn on_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.rendered || self.model.read(cx).is_empty() {
            self.ticking = false;
            return false;
        }
        self.rendered = false;
        self.ticks += 1;
        cx.notify();
        true
    }

    fn row(&self, ix: usize, item: &AttentionItem, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let compact = self.width == WidthClass::Compact;
        let mark = item.agent.as_deref().map_or(AgentMark::Unknown, AgentMark::from_name);
        let lead: AnyElement = match (&item.agent, mark) {
            (Some(_), m) => agent_icon(m, 16.0, c.muted).into_any_element(),
            (None, _) => icon(Icon::Terminal, 16.0, c.muted).into_any_element(),
        };
        let kind_color: Hsla = match &item.kind {
            AttentionKind::Approval { .. } => ink(&theme, 11, 3),
            AttentionKind::WaitingInput => ink(&theme, 12, 4),
            AttentionKind::Failed { .. } | AttentionKind::AgentError => c.error.into(),
            AttentionKind::LongCommandDone => ink(&theme, 10, 2),
        };
        let id = item.id.clone();
        let dismiss_id = item.id.clone();
        let elapsed = SharedString::from(format_wait(item.since.elapsed().as_secs()));
        div()
            .id(("attention-row", ix))
            .w_full()
            .min_h(px(if compact { 44.0 } else { 52.0 }))
            .px(px(10.0))
            .py(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .rounded(px(10.0))
            .cursor_pointer()
            .hover(move |s| s.bg(c.surface_2))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.update(cx, |m, cx| m.jump(&id, cx));
            }))
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(ty.ui_size_md))
                            .text_color(c.text)
                            .child(item.owner_label.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_size(px(ty.ui_size_sm))
                            .text_color(kind_color)
                            .child(SharedString::from(item.kind.text())),
                    )
                    .when_some(item.kind.detail().filter(|_| !compact), |d, detail| {
                        d.child(
                            div()
                                .w_full()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .font_family(ty.mono_family)
                                .text_size(px(11.5))
                                .text_color(c.muted)
                                .child(SharedString::from(detail.to_owned())),
                        )
                    }),
            )
            .child(div().flex_none().text_size(px(ty.ui_size_sm)).text_color(c.muted).child(elapsed))
            .child(
                IconButton::new(format!("attention-x-{ix}"), &theme, Icon::Close)
                    .size(24.0)
                    .icon_size(12.0)
                    .tooltip("Descartar")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |m, cx| m.dismiss(&dismiss_id, cx));
                    })),
            )
            .into_any_element()
    }
}

impl Render for AttentionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rendered = true;
        self.ensure_ticking(cx);
        let theme = self.theme;
        let c = theme.colors;
        let ty = theme.typography;
        let items: Vec<AttentionItem> = self.model.read(cx).items().into_iter().cloned().collect();

        let mut list = div().id("attention-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(px(2.0)).px(px(6.0)).pb(px(8.0));
        if items.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .py(px(24.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(ty.ui_size_md))
                    .text_color(c.muted)
                    .child(icon(Icon::Check, 20.0, c.outline))
                    .child("Nadie te está esperando"),
            );
        }
        for (ix, item) in items.iter().enumerate() {
            list = list.child(self.row(ix, item, cx));
        }

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(Radius::MD))
            .font_family(ty.ui_family)
            .text_color(c.text)
            .when(self.header, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(16.0))
                        .pt(px(14.0))
                        .pb(px(8.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(SectionLabel::new(&theme, "Atención"))
                        .when(!items.is_empty(), |d| {
                            d.child(
                                div()
                                    .h(px(18.0))
                                    .min_w(px(18.0))
                                    .px(px(6.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(9.0))
                                    .bg(c.surface_3)
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(c.text)
                                    .child(SharedString::from(items.len().to_string())),
                            )
                        }),
                )
            })
            .child(list)
    }
}

/// `PanelProvider` `"attention"` (Sidebar | Right): the queue as a panel. Build the popover version
/// with [`AttentionView::new`] directly.
pub struct AttentionPanelProvider {
    model: Entity<AttentionModel>,
}

impl AttentionPanelProvider {
    pub fn new(model: Entity<AttentionModel>) -> Self {
        Self { model }
    }
}

impl PanelProvider for AttentionPanelProvider {
    fn id(&self) -> PanelId {
        PanelId::new(PANEL_ID)
    }

    fn title(&self) -> SharedString {
        "Atención".into()
    }

    fn icon(&self) -> Icon {
        Icon::Attention
    }

    fn allowed_zones(&self) -> &'static [PanelZone] {
        &[PanelZone::Sidebar, PanelZone::Right]
    }

    fn build(&self, ctx: PanelCtx, _window: &mut Window, cx: &mut App) -> AnyView {
        let model = self.model.clone();
        cx.new(|cx| AttentionView::new(model, ctx.theme, ctx.width_class, cx)).into()
    }

    fn context_changed(&self, view: &AnyView, ctx: &PanelCtx, _window: &mut Window, cx: &mut App) -> bool {
        match view.clone().downcast::<AttentionView>() {
            Ok(v) => {
                v.update(cx, |v, cx| {
                    v.set_theme(ctx.theme, cx);
                    v.set_width_class(ctx.width_class, cx);
                });
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::{FocusTarget, AttentionItem};
    use gpui::TestAppContext;
    use std::time::Instant;

    fn waiting(id: &str, age: u64) -> AttentionItem {
        AttentionItem {
            id: id.to_string().into(),
            owner_label: format!("tab {id}").into(),
            agent: Some("claude".into()),
            kind: AttentionKind::WaitingInput,
            since: Instant::now() - Duration::from_secs(age),
            focus_target: FocusTarget::new(id.to_string()),
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<AttentionModel>, Entity<AttentionView>, &mut gpui::VisualTestContext) {
        let model = cx.new(|_| AttentionModel::new());
        let m = model.clone();
        let (view, vcx) = cx.add_window_view(move |_, cx| AttentionView::new(m, Theme::dark(), WidthClass::Regular, cx));
        (model, view, vcx)
    }

    #[gpui::test]
    fn no_timer_when_nothing_to_count(cx: &mut TestAppContext) {
        let (_model, view, vcx) = setup(cx);
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(!v.tick_active(), "empty queue: no timer"));
        vcx.executor().advance_clock(Duration::from_secs(120));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert_eq!(v.ticks(), 0));
    }

    #[gpui::test]
    fn ticks_every_second_while_visible_then_stops_when_hidden(cx: &mut TestAppContext) {
        let (model, view, vcx) = setup(cx);
        model.update(vcx, |m, cx| m.upsert(waiting("a", 5), cx));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(v.tick_active(), "visible + item: ticking"));

        // Visible: every second refreshes the view (render marks it painted again).
        for _ in 0..3 {
            vcx.executor().advance_clock(Duration::from_millis(1_100));
            vcx.run_until_parked();
        }
        let visible_ticks = view.read_with(vcx, |v, _| v.ticks());
        assert!(visible_ticks >= 3, "ticks while visible: {visible_ticks}");

        // Hidden: nothing paints the view any more. The next tick stops the timer for good.
        view.update(vcx, |v, _| v.rendered = false);
        vcx.executor().advance_clock(Duration::from_millis(1_100));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(!v.tick_active(), "hidden: timer stopped"));
        let after = view.read_with(vcx, |v, _| v.ticks());
        vcx.executor().advance_clock(Duration::from_secs(60));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert_eq!(v.ticks(), after, "no more ticks while hidden"));
    }

    #[gpui::test]
    fn a_view_that_is_never_painted_never_ticks(cx: &mut TestAppContext) {
        let model = cx.new(|_| AttentionModel::new());
        let m = model.clone();
        let view = cx.new(|cx| AttentionView::new(m, Theme::dark(), WidthClass::Compact, cx));
        model.update(cx, |m, cx| m.upsert(waiting("a", 5), cx));
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(!v.tick_active());
            assert_eq!(v.ticks(), 0);
        });
    }

    #[gpui::test]
    fn old_items_use_the_slow_tick(cx: &mut TestAppContext) {
        let (model, view, vcx) = setup(cx);
        model.update(vcx, |m, cx| m.upsert(waiting("old", 600), cx));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(v.tick_active()));
        vcx.executor().advance_clock(Duration::from_secs(5));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert_eq!(v.ticks(), 0, "no 1 s tick when the text counts minutes"));
        vcx.executor().advance_clock(Duration::from_secs(11));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(v.ticks() >= 1));
    }

    #[gpui::test]
    fn the_timer_stops_when_the_queue_empties(cx: &mut TestAppContext) {
        let (model, view, vcx) = setup(cx);
        model.update(vcx, |m, cx| m.upsert(waiting("a", 5), cx));
        vcx.run_until_parked();
        model.update(vcx, |m, cx| {
            m.remove("a", cx);
        });
        vcx.executor().advance_clock(Duration::from_millis(1_100));
        vcx.run_until_parked();
        view.read_with(vcx, |v, _| assert!(!v.tick_active()));
    }

    #[gpui::test]
    fn the_provider_declares_zones_and_id(cx: &mut TestAppContext) {
        let model = cx.new(|_| AttentionModel::new());
        let p = AttentionPanelProvider::new(model);
        assert_eq!(p.id(), PanelId::new("attention"));
        assert_eq!(p.allowed_zones(), &[PanelZone::Sidebar, PanelZone::Right]);
        assert_eq!(p.title().as_ref(), "Atención");
    }
}
