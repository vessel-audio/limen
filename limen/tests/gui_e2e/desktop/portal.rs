//! The one desktop portal setting the control panel reads: the system color
//! scheme, which "Follow system" appearance follows.

use zbus::zvariant::OwnedValue;

const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
/// `color-scheme` value meaning the user prefers dark.
const PREFER_DARK: u32 = 1;

/// `org.freedesktop.portal.Settings` reporting a dark preference.
pub struct DarkSettings;

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl DarkSettings {
    /// The deprecated single read, which GPUI's portal client still calls.
    fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        self.read_one(namespace, key)
    }

    fn read_one(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        match (namespace, key) {
            (APPEARANCE, COLOR_SCHEME) => Ok(OwnedValue::from(PREFER_DARK)),
            _ => Err(zbus::fdo::Error::Failed(format!(
                "{namespace} {key} is not set"
            ))),
        }
    }

    fn read_all(
        &self,
        _namespaces: Vec<String>,
    ) -> std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>> {
        std::collections::HashMap::from([(
            APPEARANCE.to_string(),
            std::collections::HashMap::from([(
                COLOR_SCHEME.to_string(),
                OwnedValue::from(PREFER_DARK),
            )]),
        )])
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}
