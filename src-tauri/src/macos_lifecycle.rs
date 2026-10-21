use block2::RcBlock;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSWindowDidDeminiaturizeNotification, NSWindowDidMiniaturizeNotification};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use std::{cell::RefCell, ptr::NonNull};
use tauri::Manager;

thread_local! {
    static OBSERVERS: RefCell<Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(Vec::new()) };
}

pub fn install(app: &tauri::AppHandle) {
    // AppKit completes miniaturization without a Tauri resize. Observe that native
    // boundary; querying windows on every event-loop turn creates a wakeup loop.
    let center = NSNotificationCenter::defaultCenter();
    for name in unsafe {
        [
            NSWindowDidMiniaturizeNotification,
            NSWindowDidDeminiaturizeNotification,
        ]
    } {
        let handle = app.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let app = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                if let Some(main) = app.get_webview_window("main") {
                    let _ = super::sync_compact_window(&main.as_ref().window());
                }
            });
        });
        // The block captures only a Send AppHandle. The center retains the block;
        // retain its observer token until explicit removal on the UI thread at exit.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        OBSERVERS.with(|tokens| tokens.borrow_mut().push(token));
    }
}

pub fn remove() {
    let center = NSNotificationCenter::defaultCenter();
    OBSERVERS.with(|tokens| {
        for token in tokens.borrow_mut().drain(..) {
            // These are the tokens registered by this module with this center.
            unsafe {
                center.removeObserver((*token).as_ref());
            }
        }
    });
}
