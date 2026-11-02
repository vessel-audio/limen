//! The fields of one provider page and the edits tracked between loads.

use gpui_kit::component::input::InputState;
use gpui_kit::*;
use limen_core::Polishing;

/// Where the provider's requests go when its service is a server.
pub enum Address {
    /// The ASR server's origin; its service picks the route.
    Server { server: Entity<InputState> },
    /// The polisher's chat-completions URL, and whether it is used at all.
    Url {
        polishing: Polishing,
        url: Entity<InputState>,
    },
}

/// A field whose edit must survive the initial settings load.
#[derive(Debug, Clone, Copy)]
pub enum EditedField {
    Service,
    Address,
    Key,
    Model,
    Polishing,
}
