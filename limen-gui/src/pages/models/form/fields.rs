//! The edits a provider dialog tracks.

/// A field of a provider dialog the user edited.
#[derive(Debug, Clone, Copy)]
pub enum EditedField {
    /// What a custom server speaks.
    Format,
    /// A custom server's URL.
    Address,
    Key,
    Model,
}
