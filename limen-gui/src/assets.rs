//! Assets the window loads: the component icon catalog plus Limen's own mark.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The asset path of the Limen mark, drawn like any icon in the text color.
pub const MARK: &str = "limen/mark.svg";

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            MARK => Ok(Some(Cow::Borrowed(include_bytes!("../assets/mark.svg")))),
            other => gpui_kit::assets::AllAssets.load(other),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::AllAssets.list(path).map(|listed| {
            listed
                .into_iter()
                .chain(MARK.starts_with(path).then(|| MARK.into()))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource;

    use super::{Assets, MARK};

    #[test]
    fn the_mark_is_listed_where_it_loads() {
        assert_eq!(
            Assets.list("limen/").unwrap(),
            vec![gpui_kit::SharedString::from(MARK)]
        );
    }
}
