//! Stable accessibility identity for interactive elements.
//!
//! Every control exposes its element ID as its accessible ID (AT-SPI
//! `AccessibleId`), so assistive technology and the desktop E2E suite can
//! address it independently of the interface language.

use gpui_kit::SharedString;
use gpui_kit::component::button::Button;

/// A button whose accessible ID is its element ID.
pub fn button(id: impl Into<SharedString>) -> Button {
    let id = id.into();
    Button::new(id.clone()).accessibility_id(id)
}
