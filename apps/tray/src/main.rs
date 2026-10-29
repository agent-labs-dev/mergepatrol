//! Mergepatrol in the macOS menu bar: what is merging, what needs a human, what just landed.
//! The shell (GPUI, the status item, the glass dropdown) comes from jnsdls/slopify.

mod api;
mod backdrop;
mod dropdown;
mod feed;
mod keychain;
mod model;
mod panel;
mod sign_in;
mod status_item;
mod theme;

use std::io::IsTerminal;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::App;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use crate::model::{AppModel, Auth};
use crate::status_item::StatusItem;

const DEFAULT_SERVER: &str = "https://mergepatrol.ai";

fn main() {
    let mut log =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if !std::io::stderr().is_terminal() {
        log.target(env_logger::Target::Stdout);
    }
    log.init();
    // A self-hosted server, or `http://localhost:8080` in development.
    let server = std::env::var("MERGEPATROL_URL").unwrap_or_else(|_| DEFAULT_SERVER.into());
    log::info!("mergepatrol-tray starting against {server}");

    gpui_platform::application().run(move |cx: &mut App| {
        let mtm = MainThreadMarker::new().expect("GPUI runs its callback on the main thread");
        // GPUI sets the Regular policy in applicationDidFinishLaunching, which also overrides
        // LSUIElement in a bundle, so this has to run after it either way.
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let model = AppModel::init(server, cx);
        dropdown::init(model.clone(), cx);

        let (clicks_tx, mut clicks) = mpsc::unbounded();
        cx.set_global(StatusItem::new(mtm, move || {
            let _ = clicks_tx.unbounded_send(());
        }));
        cx.observe(&model, |model, cx| {
            let title = menu_title(model.read(cx));
            cx.global::<StatusItem>().set_title(&title);
        })
        .detach();
        cx.spawn(async move |cx| {
            while clicks.next().await.is_some() {
                cx.update(|cx| {
                    if let Some(anchor) = cx.global::<StatusItem>().anchor() {
                        dropdown::toggle(anchor, cx);
                    }
                });
            }
        })
        .detach();
    });
}

/// Next to the icon: PRs in flight ▸ waiting, and a warning while any needs a human.
fn menu_title(model: &AppModel) -> String {
    if !matches!(model.auth, Auth::SignedIn { .. }) {
        return String::new();
    }
    let Some(feed) = model.feed() else {
        return String::new();
    };
    let mut title = format!(" {}▸{}", feed.active, feed.waiting);
    if !feed.held.is_empty() {
        title.push_str(&format!(" ⚠{}", feed.held.len()));
    }
    title
}
