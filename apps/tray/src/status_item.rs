//! The menu bar item: an SF Symbol plus a short live title. Borrowed from slopify.

use gpui::Global;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSCellImagePosition, NSEventMask, NSImage, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSRect, NSString};

pub struct StatusItem {
    item: Retained<NSStatusItem>,
    target: Retained<ClickTarget>,
}

impl Global for StatusItem {}

impl StatusItem {
    pub fn new(mtm: MainThreadMarker, on_click: impl Fn() + 'static) -> Self {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let target = ClickTarget::new(mtm, Box::new(on_click));
        let button = item.button(mtm).expect("status items have a button");
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("arrow.triangle.merge"),
            Some(&NSString::from_str("MergeLoop")),
        );
        if let Some(image) = image {
            image.setTemplate(true);
            button.setImage(Some(&image));
            button.setImagePosition(NSCellImagePosition::ImageLeading);
        }
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(click:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown);
        Self { item, target }
    }

    pub fn set_title(&self, title: &str) {
        let mtm = MainThreadMarker::from(&*self.target);
        if let Some(button) = self.item.button(mtm) {
            button.setTitle(&NSString::from_str(title));
        }
    }

    pub fn anchor(&self) -> Option<NSRect> {
        let mtm = MainThreadMarker::from(&*self.target);
        let button = self.item.button(mtm)?;
        Some(button.window()?.frame())
    }
}

type Callback = Box<dyn Fn()>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MergeLoopTrayStatusItemTarget"]
    #[ivars = Callback]
    struct ClickTarget;

    impl ClickTarget {
        #[unsafe(method(click:))]
        fn click(&self, _sender: Option<&AnyObject>) {
            (self.ivars())();
        }
    }
);

impl ClickTarget {
    fn new(mtm: MainThreadMarker, callback: Callback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(callback);
        unsafe { msg_send![super(this), init] }
    }
}
