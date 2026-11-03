//! The edits a provider dialog tracks, and its advanced inputs.

use gpui_kit::Entity;
use gpui_kit::component::input::InputState;

/// A field of a provider dialog the user edited.
#[derive(Debug, Clone, Copy)]
pub enum EditedField {
    /// What a custom server speaks.
    Format,
    /// A custom server's URL.
    Address,
    Key,
    Model,
    /// One of the provider's advanced settings.
    Advanced,
}

/// The inputs of a provider's advanced settings.
pub struct AdvancedFields {
    pub path: Entity<InputState>,
    pub key_header: Entity<InputState>,
    pub headers: Entity<InputState>,
    pub body: Entity<InputState>,
    pub timeout: Entity<InputState>,
    pub retries: Entity<InputState>,
}

impl AdvancedFields {
    /// Every input, with what it holds.
    pub fn all(&self) -> [&Entity<InputState>; 6] {
        [
            &self.path,
            &self.key_header,
            &self.headers,
            &self.body,
            &self.timeout,
            &self.retries,
        ]
    }
}
