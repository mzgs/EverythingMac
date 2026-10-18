use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSResponder, NSWindow, NSWindowDelegate};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSURL};
use objc2_quick_look_ui::{
    QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate,
};
use std::{cell::RefCell, path::Path};

struct PreviewItems {
    item: RefCell<Option<Retained<NSURL>>>,
    window: Retained<NSWindow>,
}

define_class!(
    // NSResponder has no extra subclassing requirements; all callbacks run on AppKit's main thread.
    #[unsafe(super = NSResponder)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PreviewItems]
    struct EverythingPreviewController;

    unsafe impl NSObjectProtocol for EverythingPreviewController {}

    impl EverythingPreviewController {
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel(&self, _: &QLPreviewPanel) -> bool {
            self.ivars().item.borrow().is_some()
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel(&self, panel: &QLPreviewPanel) {
            // AppKit calls this only after assigning this controller ownership of the panel.
            unsafe {
                panel.setDataSource(Some(ProtocolObject::from_ref(self)));
                panel.setDelegate(Some(self));
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel(&self, panel: &QLPreviewPanel) {
            unsafe {
                panel.setDataSource(None);
                panel.setDelegate(None);
            }
        }
    }

    unsafe impl QLPreviewPanelDataSource for EverythingPreviewController {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        unsafe fn number_of_preview_items(&self, _: Option<&QLPreviewPanel>) -> isize {
            isize::from(self.ivars().item.borrow().is_some())
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        unsafe fn preview_item_at_index(&self, _: Option<&QLPreviewPanel>, index: isize)
            -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            if index == 0 {
                self.ivars().item.borrow().clone().map(ProtocolObject::from_retained)
            } else {
                None
            }
        }
    }

    unsafe impl NSWindowDelegate for EverythingPreviewController {}

    unsafe impl QLPreviewPanelDelegate for EverythingPreviewController {
        #[unsafe(method(previewPanel:handleEvent:))]
        unsafe fn handle_preview_event(&self, panel: Option<&QLPreviewPanel>, event: Option<&NSEvent>) -> bool {
            if let (Some(panel), Some(event)) = (panel, event) {
                if event.r#type() == NSEventType::KeyDown {
                    match event.keyCode() {
                        49 | 53 => {
                            panel.orderOut(None);
                            true
                        }
                        125 | 126 => {
                            // Let the file window's existing arrow bindings own selection and scrolling.
                            self.ivars().window.sendEvent(event);
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
    }
);

pub(crate) struct Preview {
    controller: Retained<EverythingPreviewController>,
    previous_responder: Option<Retained<NSResponder>>,
}

impl Preview {
    pub(crate) fn new() -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Quick Look must run on the main thread")?;
        let window = NSApplication::sharedApplication(mtm)
            .mainWindow()
            .ok_or("No active file window")?;
        let allocated = EverythingPreviewController::alloc(mtm).set_ivars(PreviewItems {
            item: RefCell::new(None),
            window: window.clone(),
        });
        // The controller stays retained while it is installed in the responder chain.
        unsafe {
            let controller: Retained<EverythingPreviewController> =
                msg_send![super(allocated), init];
            let previous_responder = window.nextResponder();
            controller.setNextResponder(previous_responder.as_deref());
            window.setNextResponder(Some(&controller));
            Ok(Self {
                controller,
                previous_responder,
            })
        }
    }

    pub(crate) fn toggle(&self, path: &Path) -> Result<(), String> {
        let mtm = self.controller.mtm();
        // These APIs are main-thread-only; the controller's marker guarantees that here.
        unsafe {
            let panel =
                QLPreviewPanel::sharedPreviewPanel(mtm).ok_or("Quick Look is unavailable")?;
            if panel.isVisible() && self.owns(&panel) {
                panel.orderOut(None);
                return Ok(());
            }
            *self.controller.ivars().item.borrow_mut() = Some(preview_item(path)?);
            panel.makeKeyAndOrderFront(None);
            panel.updateController();
            if !self.owns(&panel) {
                panel.orderOut(None);
                return Err("Quick Look couldn’t attach to the file window".into());
            }
            panel.reloadData();
        }
        Ok(())
    }

    pub(crate) fn update_if_visible(&self, path: &Path) -> Result<(), String> {
        let mtm = self.controller.mtm();
        unsafe {
            if QLPreviewPanel::sharedPreviewPanelExists(mtm)
                && let Some(panel) = QLPreviewPanel::sharedPreviewPanel(mtm)
                && panel.isVisible()
                && self.owns(&panel)
            {
                *self.controller.ivars().item.borrow_mut() = Some(preview_item(path)?);
                panel.reloadData();
            }
        }
        Ok(())
    }

    fn owns(&self, panel: &QLPreviewPanel) -> bool {
        unsafe {
            panel
                .currentController()
                .as_ref()
                .is_some_and(|controller| {
                    Retained::as_ptr(controller).cast::<EverythingPreviewController>()
                        == Retained::as_ptr(&self.controller)
                })
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        // Detach before releasing the controller: Quick Look and NSResponder don't retain their delegates.
        unsafe {
            let mtm = self.controller.mtm();
            let panel = QLPreviewPanel::sharedPreviewPanelExists(mtm)
                .then(|| QLPreviewPanel::sharedPreviewPanel(mtm))
                .flatten();
            if let Some(panel) = &panel {
                if self.owns(panel) {
                    panel.setDataSource(None);
                    panel.setDelegate(None);
                    panel.orderOut(None);
                }
            }
            self.controller
                .ivars()
                .window
                .setNextResponder(self.previous_responder.as_deref());
            if let Some(panel) = panel {
                panel.updateController();
            }
        }
    }
}

fn preview_item(path: &Path) -> Result<Retained<NSURL>, String> {
    std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    NSURL::from_file_path(path).ok_or_else(|| "Invalid file path".into())
}

#[cfg(test)]
mod tests {
    use super::preview_item;

    #[test]
    fn preview_items_preserve_file_urls_and_reject_missing_paths() {
        let path = std::env::current_dir().unwrap().join("Cargo.toml");
        assert_eq!(preview_item(&path).unwrap().to_file_path(), Some(path));
        assert!(preview_item(std::path::Path::new("/invalid\0file")).is_err());
        assert!(
            preview_item(
                &std::env::temp_dir().join(format!("missing-preview-{}", std::process::id()))
            )
            .is_err()
        );
    }
}
