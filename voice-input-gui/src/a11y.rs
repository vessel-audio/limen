//! Stable accessibility identity for interactive elements.
//!
//! Controls and the status they report expose their element ID as their
//! accessible ID (AT-SPI `AccessibleId`), so assistive technology and the
//! desktop E2E suite can address them independently of the interface
//! language. The lightweight switch and the font select are the exceptions:
//! their components build the semantic node internally without an ID hook,
//! and each is the only control of its role in the window.
//!
//! Status text is a live region, so screen readers announce its changes
//! without the user moving focus to it.

use gpui_kit::accesskit::Live;
use gpui_kit::component::button::Button;
use gpui_kit::*;

/// A button whose accessible ID is its element ID.
pub fn button(id: impl Into<SharedString>) -> Button {
    let id = id.into();
    Button::new(id.clone()).accessibility_id(id)
}

/// Status text announced politely when it changes.
pub fn status(id: &'static str, text: impl Into<SharedString>) -> Stateful<Div> {
    live(id, Role::Status, text.into(), Live::Polite)
}

/// Urgent text announced as soon as it appears.
pub fn alert(id: &'static str, text: impl Into<SharedString>) -> Stateful<Div> {
    live(id, Role::Alert, text.into(), Live::Assertive)
}

fn live(id: &'static str, role: Role, text: SharedString, politeness: Live) -> Stateful<Div> {
    div()
        .id(id)
        .role(role)
        .accessibility_id(id)
        .aria_label(text)
        .a11y_synthetic_children(move |tree| tree.parent_node().set_live(politeness))
}
