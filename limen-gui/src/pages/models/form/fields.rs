//! The fields of one provider page and the edits tracked between loads.

use gpui_kit::component::input::InputState;
use gpui_kit::*;
use limen_core::Polishing;

use super::EndpointSelect;

/// Where the provider's requests go.
pub enum Address {
    /// The ASR server's origin and one of the known routes.
    Server {
        server: Entity<InputState>,
        route: Entity<EndpointSelect>,
    },
    /// The polisher's chat-completions URL, and whether it is used at all.
    Url {
        polishing: Polishing,
        url: Entity<InputState>,
    },
}

/// A field whose edit must survive the initial settings load.
#[derive(Debug, Clone, Copy)]
pub enum EditedField {
    Address,
    Route,
    Key,
    Model,
    Polishing,
}
