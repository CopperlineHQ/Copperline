// SPDX-License-Identifier: GPL-3.0-or-later

//! Route winit's standard macOS About menu item to Copperline's live panel.

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol};
use std::sync::atomic::{AtomicBool, Ordering};
use winit::event_loop::EventLoopProxy;

static REQUESTED: AtomicBool = AtomicBool::new(false);

struct MenuTargetIvars {
    wake: EventLoopProxy<()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the menu target is
    // kept alive for the lifetime of the application.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct CopperlineAboutMenuTarget;

    impl CopperlineAboutMenuTarget {
        #[unsafe(method(showCopperlineAbout:))]
        fn show_about(&self, _sender: &objc2::runtime::AnyObject) {
            REQUESTED.store(true, Ordering::Release);
            let _ = self.ivars().wake.send_event(());
        }
    }

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for CopperlineAboutMenuTarget {}
);

impl CopperlineAboutMenuTarget {
    fn new(mtm: MainThreadMarker, wake: EventLoopProxy<()>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { wake });
        // SAFETY: NSObject's init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

pub(super) fn install(wake: EventLoopProxy<()>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let about_item = app
        .mainMenu()
        .and_then(|menu| menu.itemAtIndex(0))
        .and_then(|item| item.submenu())
        .and_then(|menu| menu.itemAtIndex(0));
    let Some(item) = about_item else {
        return;
    };
    if item.action() != Some(sel!(orderFrontStandardAboutPanel:)) {
        return;
    }
    let target = CopperlineAboutMenuTarget::new(mtm, wake);
    // SAFETY: The selector is implemented by target. AppKit holds a weak
    // target reference, so we intentionally retain it until process exit.
    unsafe {
        item.setTarget(Some(&target));
        item.setAction(Some(sel!(showCopperlineAbout:)));
    }
    std::mem::forget(target);
}

pub(super) fn take_request() -> bool {
    REQUESTED.swap(false, Ordering::AcqRel)
}
