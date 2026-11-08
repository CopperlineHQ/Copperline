// SPDX-License-Identifier: GPL-3.0-or-later

//! Finder open-document events. Winit 0.30 owns NSApplication's delegate, so
//! register an Apple-event handler instead of replacing that delegate.

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSNotification,
    NSNotificationCenter, NSObject, NSObjectProtocol,
};
use std::cell::RefCell;
use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use winit::event_loop::EventLoopProxy;

const CORE_EVENT: u32 = u32::from_be_bytes(*b"aevt");
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct HandlerIvars {
    wake: EventLoopProxy<()>,
    pending: RefCell<Vec<Vec<PathBuf>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The event manager
    // dispatches to this retained target on the application main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = HandlerIvars]
    struct CopperlineDocumentHandler;

    impl CopperlineDocumentHandler {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            // AppKit installs its own core-event handlers during launch.
            // Register again here, before the initial document is delivered.
            self.register();
        }

        #[unsafe(method(openDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let paths = document_paths(event);
            log::debug!("macOS open-document event: {} file(s)", paths.len());
            if !paths.is_empty() {
                self.ivars().pending.borrow_mut().push(paths);
                let _ = self.ivars().wake.send_event(());
            }
        }
    }

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for CopperlineDocumentHandler {}
);

impl CopperlineDocumentHandler {
    fn register(&self) {
        log::debug!("registering macOS open-document handler");
        // SAFETY: The selector has NSAppleEventManager's two-descriptor ABI
        // and this target remains retained for the event loop's lifetime.
        unsafe {
            NSAppleEventManager::sharedAppleEventManager()
                .setEventHandler_andSelector_forEventClass_andEventID(
                    self,
                    sel!(openDocuments:withReplyEvent:),
                    CORE_EVENT,
                    OPEN_DOCUMENTS,
                );
        }
    }
}

fn document_paths(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    let Some(list) = event.paramDescriptorForKeyword(DIRECT_OBJECT) else {
        return Vec::new();
    };
    (1..=list.numberOfItems())
        .filter_map(|index| list.descriptorAtIndex(index))
        .filter_map(|item| item.fileURLValue())
        .filter(|url| url.isFileURL())
        .map(|url| {
            // SAFETY: NSURL owns a NUL-terminated filesystem representation;
            // copy it while the URL is retained, preserving Unicode and bytes.
            let bytes = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
            PathBuf::from(OsStr::from_bytes(bytes.to_bytes()))
        })
        .collect()
}

pub(super) struct Handler(Retained<CopperlineDocumentHandler>);

impl Handler {
    pub(super) fn install(wake: EventLoopProxy<()>) -> Self {
        let mtm = MainThreadMarker::new().expect("winit runs on the main thread");
        let target = CopperlineDocumentHandler::alloc(mtm).set_ivars(HandlerIvars {
            wake,
            pending: RefCell::default(),
        });
        // SAFETY: NSObject's init signature is correct; the handler implements
        // the selector with NSAppleEventManager's required two-descriptor ABI.
        let target: Retained<CopperlineDocumentHandler> = unsafe { msg_send![super(target), init] };
        // SAFETY: The observer implements this notification selector and is
        // removed before its retained target is released.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &target,
                sel!(applicationWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
        target.register();
        Self(target)
    }

    pub(super) fn take_requests(&self) -> Vec<Vec<PathBuf>> {
        std::mem::take(&mut *self.0.ivars().pending.borrow_mut())
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        // SAFETY: The observer is still retained and is removed on the same
        // main thread on which it was registered.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(&self.0) };
        NSAppleEventManager::sharedAppleEventManager()
            .removeEventHandlerForEventClass_andEventID(CORE_EVENT, OPEN_DOCUMENTS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSURL;

    #[test]
    fn finder_document_lists_preserve_spaces_unicode_and_order() {
        // NSURL returns macOS's decomposed filesystem representation.
        let paths = [
            "/tmp/Copperline disk 1.adf",
            "/tmp/Copperline cafe\u{301}.adf",
        ];
        let event = NSAppleEventDescriptor::recordDescriptor();
        let list = NSAppleEventDescriptor::listDescriptor();
        for path in paths {
            let url = NSURL::fileURLWithPath(&objc2_foundation::NSString::from_str(path));
            let item = NSAppleEventDescriptor::descriptorWithFileURL(&url);
            list.insertDescriptor_atIndex(&item, 0);
        }
        event.setParamDescriptor_forKeyword(&list, DIRECT_OBJECT);
        assert_eq!(document_paths(&event), paths.map(PathBuf::from));
        assert!(document_paths(&NSAppleEventDescriptor::recordDescriptor()).is_empty());
    }
}
