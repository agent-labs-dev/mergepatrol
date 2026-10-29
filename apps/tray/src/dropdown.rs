use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Entity, Global, KeyBinding, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, actions, point, px, size,
};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::backdrop;
use crate::model::AppModel;
use crate::panel::Panel;

pub const WIDTH: f64 = 360.0;
const MIN_HEIGHT: f64 = 120.0;
const MAX_HEIGHT: f64 = 680.0;
const INITIAL_HEIGHT: f64 = 400.0;
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(200);

actions!(dropdown, [Dismiss]);

/// The Dropdown window. Created hidden at launch and never closed, because the UI it holds
/// outlives any one showing.
struct Dropdown {
    window: WindowHandle<Panel>,
    ns_window: Retained<NSWindow>,
    /// Read when the deferred AppKit calls run, not when they are queued: the content can be
    /// measured between the two, and the last frame set must use the latest height.
    height: Rc<Cell<f64>>,
    hidden_at: Option<Instant>,
}

impl Global for Dropdown {}

pub fn init(model: Entity<AppModel>, cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Dropdown"))]);

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(WIDTH as f32), px(INITIAL_HEIGHT as f32)),
        })),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let window = cx
        .open_window(options, |window, cx| {
            cx.new(|cx| Panel::new(model, window, cx))
        })
        .expect("the Dropdown window opens");
    let ns_window = window
        .update(cx, |_, window, cx| {
            // The frameless window has no close button, but Cmd-W and AppKit can still ask.
            window.on_window_should_close(cx, |_, cx| {
                hide(cx);
                false
            });
            ns_window(window)
        })
        .expect("the Dropdown window is alive");

    // GPUI's PopUp kind already joins all spaces and floats over full-screen apps; this keeps
    // the Dropdown out of Mission Control too.
    ns_window.setCollectionBehavior(
        ns_window.collectionBehavior() | NSWindowCollectionBehavior::Transient,
    );
    backdrop::install(&ns_window);

    cx.set_global(Dropdown {
        window,
        ns_window,
        height: Rc::new(Cell::new(INITIAL_HEIGHT)),
        hidden_at: None,
    });
}

/// Shows the Dropdown under `anchor`, or hides it if it is showing.
pub fn toggle(anchor: NSRect, cx: &mut App) {
    let dropdown = cx.global::<Dropdown>();
    if dropdown.ns_window.isVisible() {
        hide(cx);
    } else if dropdown
        .hidden_at
        .is_none_or(|at| at.elapsed() > TOGGLE_DEBOUNCE)
    {
        // A click on the icon while the Dropdown is key first blurs it (hiding it) and then
        // arrives here; without the debounce that click would reopen it.
        show(anchor, cx);
    }
}

fn show(anchor: NSRect, cx: &mut App) {
    let dropdown = cx.global::<Dropdown>();
    let height = dropdown.height.clone();
    let window = dropdown.window;
    let _ = window.update(cx, |panel, window, cx| panel.on_show(window, cx));
    with_ns_window(cx, move |ns_window| {
        ns_window.setFrame_display(position_under(anchor, height.get(), ns_window), true);
        // Activating, as Electron's focus() does, keeps the Dropdown key. A key panel of an
        // inactive app blurs as soon as the frontmost app touches its own windows.
        let app = NSApplication::sharedApplication(MainThreadMarker::from(ns_window));
        app.unhideWithoutActivation();
        #[allow(deprecated, reason = "activate() needs macOS 14")]
        app.activateIgnoringOtherApps(true);
        ns_window.makeKeyAndOrderFront(None);
    });
}

pub fn hide(cx: &mut App) {
    let dropdown = cx.global_mut::<Dropdown>();
    if !dropdown.ns_window.isVisible() {
        return;
    }
    dropdown.hidden_at = Some(Instant::now());
    with_ns_window(cx, |ns_window| {
        ns_window.orderOut(None);
        // Hands keyboard focus back to the previous app when Escape or the icon closes it.
        let app = NSApplication::sharedApplication(MainThreadMarker::from(ns_window));
        if app.isActive() {
            app.hide(None);
        }
    });
}

pub fn app_is_active() -> bool {
    let mtm = MainThreadMarker::new().expect("GPUI runs window callbacks on the main thread");
    NSApplication::sharedApplication(mtm).isActive()
}

/// Resizes the Dropdown to fit its content, keeping its top edge where it is.
pub fn set_content_height(px: f64, cx: &mut App) {
    // The first frame is drawn while `init` is still building the window.
    if !px.is_finite() || !cx.has_global::<Dropdown>() {
        return;
    }
    let height = cx.global::<Dropdown>().height.clone();
    height.set(px.clamp(MIN_HEIGHT, MAX_HEIGHT).round());
    with_ns_window(cx, move |ns_window| {
        let height = height.get();
        let current = ns_window.frame();
        let top = current.origin.y + current.size.height;
        let frame = NSRect::new(
            NSPoint::new(current.origin.x, top - height),
            NSSize::new(WIDTH, height),
        );
        ns_window.setFrame_display(clamp_to_work_area(frame, ns_window), true);
    });
    // GPUI (at the pinned rev) does not redraw after a resize it did not start: its last frame,
    // laid out for the old size, stays squeezed into the new one. A refresh once AppKit has
    // resized the view redraws it; a refresh on the same turn comes too early.
    let window = cx.global::<Dropdown>().window;
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        cx.update(|cx| {
            let _ = window.update(cx, |_, window, _| window.refresh());
        });
    })
    .detach();
}

/// AppKit answers these calls with synchronous delegate callbacks into GPUI, which fail while
/// the app is borrowed, so they run on a fresh main-loop turn the way GPUI's own window calls do.
fn with_ns_window(cx: &App, f: impl FnOnce(&NSWindow) + 'static) {
    let ns_window = cx.global::<Dropdown>().ns_window.clone();
    cx.foreground_executor()
        .spawn(async move { f(&ns_window) })
        .detach();
}

// AppKit screen coordinates have their origin at the bottom left, so "under" means a smaller y.
fn position_under(anchor: NSRect, height: f64, window: &NSWindow) -> NSRect {
    let x = (anchor.origin.x + anchor.size.width / 2.0 - WIDTH / 2.0).round();
    let top = anchor.origin.y;
    let frame = NSRect::new(NSPoint::new(x, top - height), NSSize::new(WIDTH, height));
    clamp_to_work_area(frame, window)
}

fn clamp_to_work_area(frame: NSRect, window: &NSWindow) -> NSRect {
    let top_left = NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height);
    let screens = NSScreen::screens(MainThreadMarker::from(window));
    let nearest = screens
        .iter()
        .min_by(|a, b| distance(top_left, a.frame()).total_cmp(&distance(top_left, b.frame())));
    match nearest {
        Some(screen) => clamp(frame, screen.visibleFrame()),
        None => frame,
    }
}

/// Keeps `frame` inside `area`, shortening it if it is taller. The top edge wins over the bottom.
fn clamp(frame: NSRect, area: NSRect) -> NSRect {
    let height = frame.size.height.min(area.size.height);
    let top = frame.origin.y + frame.size.height;
    let x = frame
        .origin
        .x
        .min(area.origin.x + area.size.width - frame.size.width)
        .max(area.origin.x);
    let y = (top - height)
        .max(area.origin.y)
        .min(area.origin.y + area.size.height - height);
    NSRect::new(NSPoint::new(x, y), NSSize::new(frame.size.width, height))
}

fn distance(point: NSPoint, rect: NSRect) -> f64 {
    let dx = (rect.origin.x - point.x)
        .max(point.x - (rect.origin.x + rect.size.width))
        .max(0.0);
    let dy = (rect.origin.y - point.y)
        .max(point.y - (rect.origin.y + rect.size.height))
        .max(0.0);
    dx.hypot(dy)
}

fn ns_window(window: &Window) -> Retained<NSWindow> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
        .expect("GPUI windows have a handle")
        .as_raw()
    else {
        unreachable!("the prototype only runs on macOS");
    };
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window().expect("the GPUI view sits in a window")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    // A 1512x982 screen whose menu bar takes the top 24 pt.
    const AREA: NSRect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 958.0));

    #[test]
    fn leaves_a_fitting_frame_alone() {
        assert_eq!(
            clamp(rect(600.0, 558.0, 300.0, 400.0), AREA),
            rect(600.0, 558.0, 300.0, 400.0)
        );
    }

    #[test]
    fn pulls_a_frame_off_the_right_edge() {
        assert_eq!(
            clamp(rect(1400.0, 558.0, 300.0, 400.0), AREA),
            rect(1212.0, 558.0, 300.0, 400.0)
        );
    }

    #[test]
    fn keeps_the_top_under_the_menu_bar() {
        assert_eq!(
            clamp(rect(600.0, 600.0, 300.0, 400.0), AREA),
            rect(600.0, 558.0, 300.0, 400.0)
        );
    }

    #[test]
    fn shortens_a_frame_taller_than_the_screen() {
        let short = rect(0.0, 0.0, 1512.0, 300.0);
        assert_eq!(
            clamp(rect(600.0, -2.0, 300.0, 560.0), short),
            rect(600.0, 0.0, 300.0, 300.0)
        );
    }

    #[test]
    fn measures_zero_distance_inside_a_rect() {
        assert_eq!(distance(NSPoint::new(10.0, 10.0), AREA), 0.0);
        assert_eq!(distance(NSPoint::new(1515.0, 962.0), AREA), 5.0);
    }
}
