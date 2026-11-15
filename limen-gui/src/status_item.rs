//! The macOS menu-bar status item. GPUI owns `NSApplication` and the main
//! run loop but has no status-item API, so this talks to AppKit directly:
//! an `NSStatusItem` with a three-entry menu whose picks are forwarded over
//! a channel to whoever drives the window and the bridge.

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::NSString;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// What the user picked from the status item's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuEvent {
    ToggleDictation,
    OpenWindow,
    Quit,
}

/// What the status item's icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Indicator {
    /// No daemon answers.
    Offline,
    Idle,
    Recording,
    Transcribing,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `MenuTarget`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LimenStatusItemTarget"]
    #[ivars = UnboundedSender<MenuEvent>]
    /// The menu items' action target: each action forwards its
    /// [`MenuEvent`]. A closed channel means nobody listens any more, so a
    /// failed send is dropped.
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(toggleDictation:))]
        fn toggle_dictation(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().send(MenuEvent::ToggleDictation);
        }

        #[unsafe(method(openWindow:))]
        fn open_window(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().send(MenuEvent::OpenWindow);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().send(MenuEvent::Quit);
        }
    }

    // SAFETY: `NSObjectProtocol` has no requirements beyond those `NSObject`
    // already meets.
    unsafe impl NSObjectProtocol for MenuTarget {}
);

/// Main-thread owner of the `NSStatusItem`; dropping it removes the item
/// from the menu bar.
pub(crate) struct StatusItem {
    item: Retained<NSStatusItem>,
    /// The status item retains its menu; held here so the ownership is
    /// spelled out next to the target's.
    _menu: Retained<NSMenu>,
    /// `NSMenuItem.target` is weak: without this the actions would have
    /// nobody to call.
    _target: Retained<MenuTarget>,
    mtm: MainThreadMarker,
}

impl StatusItem {
    /// Must be called on the main thread after `NSApplication` exists.
    pub(crate) fn install() -> Result<(Self, UnboundedReceiver<MenuEvent>)> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow!("the status item must be installed on the main thread"))?;
        let (sender, events) = unbounded_channel();
        let target = MenuTarget::alloc(mtm).set_ivars(sender);
        // SAFETY: `init` is NSObject's designated initializer, sent to the
        // superclass of a freshly allocated object whose ivars are set.
        let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
        let entry = |title: &str, action: Sel| {
            // SAFETY: `action` is one of the selectors `MenuTarget` defines.
            let entry = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::new(),
                )
            };
            // SAFETY: the target implements the entry's action with the
            // `(id sender)` signature AppKit calls it with, and `StatusItem`
            // keeps the target alive for as long as the menu exists.
            unsafe { entry.setTarget(Some(&target)) };
            entry
        };
        let menu = NSMenu::new(mtm);
        menu.addItem(&entry(
            &rust_i18n::t!("status_item.toggle"),
            sel!(toggleDictation:),
        ));
        menu.addItem(&entry(
            &rust_i18n::t!("status_item.open"),
            sel!(openWindow:),
        ));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&entry(&rust_i18n::t!("status_item.quit"), sel!(quit:)));
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        item.setMenu(Some(&menu));
        let status_item = Self {
            item,
            _menu: menu,
            _target: target,
            mtm,
        };
        status_item.show(Indicator::Idle);
        Ok((status_item, events))
    }

    /// Swap the icon.
    pub(crate) fn show(&self, indicator: Indicator) {
        let symbol = match indicator {
            Indicator::Offline => "mic.slash",
            Indicator::Idle => "mic",
            Indicator::Recording => "mic.fill",
            Indicator::Transcribing => "waveform",
        };
        let title = NSString::from_str(&rust_i18n::t!("app.title"));
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            Some(&title),
        );
        match (self.item.button(self.mtm), image) {
            (Some(button), Some(image)) => {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            // No such symbol on this system: a text item beats an invisible one.
            (Some(button), None) => button.setTitle(&title),
            (None, Some(_) | None) => {}
        }
    }
}

/// Show the app's windows again after `NSApplication.hide`. GPUI can hide
/// the app but not unhide it. Main thread only; elsewhere it does nothing.
pub(crate) fn unhide_app() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).unhide(None)
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
