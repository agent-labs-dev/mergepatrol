//! What the dropdown shows: the feed when signed in, a sign-in button when not.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontFeatures, FontWeight, Hsla,
    Pixels, Render, SharedString, Stateful, Subscription, Window, canvas, div, prelude::*, px,
    relative,
};

use crate::dropdown::{self, Dismiss, MAX_HEIGHT, WIDTH};
use crate::feed::{self, Feed, NowCard, Stage};
use crate::model::{AppModel, Auth};
use crate::theme::Theme;

pub struct Panel {
    focus: FocusHandle,
    model: Entity<AppModel>,
    measured: Rc<Cell<Option<Pixels>>>,
    _subscriptions: Vec<Subscription>,
}

impl Panel {
    pub fn new(model: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe_window_activation(window, |_, window, cx| {
                // AppKit can take key from the panel while the app is still active; only a blur
                // that leaves the app inactive is a click outside.
                if !window.is_window_active() && !dropdown::app_is_active() {
                    dropdown::hide(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            cx.observe(&model, |_, _, cx| cx.notify()),
        ];
        Self {
            focus: cx.focus_handle(),
            model,
            measured: Rc::default(),
            _subscriptions: subscriptions,
        }
    }

    pub fn on_show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.measured.set(None);
        cx.notify();
    }

    fn signed_out(
        &self,
        error: Option<&str>,
        signing_in: bool,
        t: Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let server = self.model.read(cx).server.clone();
        let button = div()
            .id("sign-in")
            .mt(px(14.))
            .px(px(14.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(t.pill_bg)
            .text_color(t.pill_fg)
            .font_weight(FontWeight::MEDIUM)
            .child(if signing_in {
                "Signing in…"
            } else {
                "Sign in with GitHub"
            });
        let button = if signing_in {
            button.opacity(0.6)
        } else {
            button
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.model.update(cx, |m, cx| m.sign_in(cx));
                }))
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .px(px(20.))
            .pt(px(28.))
            .pb(px(18.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Mergepatrol"),
            )
            .child(
                div()
                    .mt(px(4.))
                    .text_color(t.muted)
                    .text_center()
                    .child("See what your merge queues are doing."),
            )
            .children(error.map(|e| {
                div()
                    .mt(px(10.))
                    .text_color(t.bad)
                    .text_center()
                    .child(SharedString::from(e.to_owned()))
            }))
            .child(button)
            .child(
                div()
                    .mt(px(12.))
                    .text_size(px(11.))
                    .text_color(t.faint)
                    .child(server),
            )
    }

    fn feed(&self, feed: &Feed, t: Theme, cx: &mut Context<Self>) -> Div {
        let mut col = div().flex().flex_col().child(label_row("Merging now", t));
        if feed.now.is_empty() {
            col = col.child(empty_row("Nothing is merging.", t));
        }
        for (i, card) in feed.now.iter().enumerate() {
            col = col.child(self.now_card(i, card, t, cx));
        }
        if !feed.held.is_empty() {
            col = col.child(label_row("Needs a human", t));
            for (i, r) in feed.held.iter().enumerate() {
                col = col.child(self.row(
                    ("held", i),
                    glyph("⏸", t.warn),
                    &r.text,
                    r.repo.clone(),
                    &r.url,
                    t,
                    cx,
                ));
            }
        }
        if !feed.next.is_empty() {
            col = col.child(label_row("Up next", t));
            for (i, r) in feed.next.iter().enumerate() {
                col = col.child(self.row(
                    ("next", i),
                    glyph("○", t.faint),
                    &r.text,
                    r.repo.clone(),
                    &r.url,
                    t,
                    cx,
                ));
            }
        }
        col = col.child(label_row("Just landed", t));
        if feed.landed.is_empty() {
            col = col.child(empty_row("Nothing left the queue today.", t));
        }
        for (i, x) in feed.landed.iter().enumerate() {
            let (g, color) = if x.merged {
                ("✓", t.good)
            } else {
                ("✕", t.bad)
            };
            let right = match x.took {
                Some(took) => format!("{} · {} ago", feed::duration(took), feed::duration(x.ago)),
                None => format!("{} ago", feed::duration(x.ago)),
            };
            col = col.child(self.row(
                ("landed", i),
                glyph(g, color),
                &x.text,
                right,
                &x.url,
                t,
                cx,
            ));
        }
        col
    }

    fn now_card(
        &self,
        i: usize,
        card: &NowCard,
        t: Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let color = stage_color(card.stage, t);
        let right = match (card.elapsed, card.typical) {
            (Some(e), Some(p50)) if e < p50 => format!("~{} left", feed::duration(p50 - e)),
            (Some(e), Some(_)) => format!("{} · over typical", feed::duration(e)),
            (Some(e), None) => feed::duration(e),
            (None, _) => String::new(),
        };
        let fraction = match (card.elapsed, card.typical) {
            (Some(e), Some(p50)) if p50 > 0.0 => (e / p50).min(1.0) as f32,
            _ => 0.0,
        };
        let over = matches!((card.elapsed, card.typical), (Some(e), Some(p50)) if e > p50);
        let url = card.url.clone();
        div()
            .id(("now", i))
            .mx(px(10.))
            .mb(px(6.))
            .p(px(10.))
            .rounded(px(10.))
            .bg(t.chip)
            .cursor_pointer()
            .hover(move |s| s.bg(t.chip_hover))
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .text_color(t.muted)
                    .text_size(px(11.))
                    .child(card.repo.clone())
                    .child(div().flex_1())
                    .child(right),
            )
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(card.what.clone()),
            )
            .child(
                div().h(px(3.)).rounded(px(2.)).bg(t.track).child(
                    div()
                        .h_full()
                        .rounded(px(2.))
                        .w(relative(fraction))
                        .bg(if over { t.warn } else { color }),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_none()
                            .px(px(6.))
                            .rounded(px(4.))
                            .bg(color.opacity(0.18))
                            .text_color(color)
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(card.stage.label()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_color(t.muted)
                            .child(card.note.clone()),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments, reason = "one call site per section")]
    fn row(
        &self,
        id: (&'static str, usize),
        lead: AnyElement,
        text: &str,
        right: String,
        url: &str,
        t: Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let url = url.to_owned();
        div()
            .id(id)
            .mx(px(6.))
            .px(px(10.))
            .py(px(3.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |s| s.bg(t.chip))
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(text.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(t.faint)
                    .text_size(px(11.))
                    .child(right),
            )
    }

    fn footer(&self, t: Theme, cx: &mut Context<Self>) -> Div {
        let model = self.model.read(cx);
        let status = match (&model.auth, &model.error, model.fetched_at) {
            (Auth::SignedIn { login, .. }, Some(_), _) => {
                format!("{login} · can't reach the server")
            }
            (Auth::SignedIn { login, .. }, None, Some(at)) => {
                format!(
                    "{login} · updated {} ago",
                    feed::duration(at.elapsed().as_secs_f64())
                )
            }
            (Auth::SignedIn { login, .. }, None, None) => format!("{login} · loading…"),
            _ => String::new(),
        };
        let signed_in = matches!(model.auth, Auth::SignedIn { .. });
        let dashboard = model.server.clone();
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .text_color(t.muted)
                .hover(|s| s.text_color(t.fg))
                .child(label)
        };
        div()
            .mt(px(8.))
            .mx(px(16.))
            .py(px(8.))
            .border_t_1()
            .border_color(t.line)
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(11.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if model.error.is_some() {
                        t.warn
                    } else {
                        t.faint
                    })
                    .child(status),
            )
            .children(signed_in.then(|| {
                link("dashboard", "Dashboard")
                    .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&dashboard)))
            }))
            .children(signed_in.then(|| {
                link("sign-out", "Sign out").on_click(cx.listener(|this, _, _, cx| {
                    this.model.update(cx, |m, cx| m.sign_out(cx));
                }))
            }))
            .child(link("quit", "Quit").on_click(|_, _, cx| cx.quit()))
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        // As tall as the window may get: `dropdown` also keeps it inside the screen's work area.
        let max_height = window.display(cx).map_or(px(MAX_HEIGHT as f32), |d| {
            d.visible_bounds().size.height.min(px(MAX_HEIGHT as f32))
        });
        let model = self.model.read(cx);
        let body = match &model.auth {
            Auth::SignedOut { error } => {
                let error = error.clone();
                self.signed_out(error.as_deref(), false, t, cx)
            }
            Auth::SigningIn => self.signed_out(None, true, t, cx),
            Auth::SignedIn { .. } => match model.feed() {
                Some(feed) => self.feed(&feed, t, cx),
                None => div().child(empty_row("Loading…", t)),
            },
        };

        let measured = self.measured.clone();
        let measure = canvas(
            move |bounds, _, cx| {
                let height = bounds.size.height;
                if measured.get() != Some(height) {
                    measured.set(Some(height));
                    dropdown::set_content_height(f64::from(f32::from(height)), cx);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        div()
            .key_context("Dropdown")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| dropdown::hide(cx)))
            .size_full()
            .bg(t.tint)
            .text_color(t.fg)
            .font_family(".SystemUIFont")
            .text_size(px(12.))
            .line_height(relative(1.35))
            .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(WIDTH as f32))
                    .max_h(max_height)
                    .flex()
                    .flex_col()
                    .child(div().id("body").min_h_0().overflow_y_scroll().child(body))
                    .child(self.footer(t, cx).flex_none())
                    .child(measure),
            )
    }
}

impl Focusable for Panel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn stage_color(stage: Stage, t: Theme) -> Hsla {
    match stage {
        Stage::Starting => t.faint,
        Stage::Rebasing | Stage::Checks => t.accent,
        Stage::Fixing => t.bad,
        Stage::Held => t.warn,
    }
}

fn glyph(g: &'static str, color: Hsla) -> AnyElement {
    div()
        .flex_none()
        .w(px(12.))
        .text_color(color)
        .child(g)
        .into_any_element()
}

fn label_row(title: &'static str, t: Theme) -> Div {
    div()
        .px(px(16.))
        .pt(px(12.))
        .pb(px(4.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.muted)
        .child(title)
}

fn empty_row(text: &'static str, t: Theme) -> Div {
    div().px(px(16.)).py(px(3.)).text_color(t.faint).child(text)
}
