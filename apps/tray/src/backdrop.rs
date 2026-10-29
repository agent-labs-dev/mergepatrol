//! The native material behind the Dropdown: Liquid Glass on macOS 26 and later, the popover
//! vibrancy before that. GPUI draws on a transparent window above it.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, available};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSGlassEffectView, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowOrderingMode,
};
use objc2_foundation::NSRect;

const CORNER_RADIUS: f64 = 16.0;

pub fn install(ns_window: &NSWindow) {
    let content = ns_window
        .contentView()
        .expect("the Dropdown has a content view");
    let backdrop = backdrop(content.bounds(), MainThreadMarker::from(ns_window));
    backdrop.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    content.addSubview_positioned_relativeTo(&backdrop, NSWindowOrderingMode::Below, None);

    // Rounds GPUI's layer with the material, and the window shadow with both.
    content.setWantsLayer(true);
    if let Some(layer) = content.layer() {
        layer.setCornerRadius(CORNER_RADIUS);
        layer.setMasksToBounds(true);
    }
    ns_window.setHasShadow(true);
    ns_window.invalidateShadow();
}

fn backdrop(frame: NSRect, mtm: MainThreadMarker) -> Retained<NSView> {
    if available!(macos = 26.0) {
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(CORNER_RADIUS);
        Retained::into_super(glass)
    } else {
        let vibrancy = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        vibrancy.setMaterial(NSVisualEffectMaterial::Popover);
        vibrancy.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        vibrancy.setState(NSVisualEffectState::Active);
        Retained::into_super(vibrancy)
    }
}
