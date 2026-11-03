//! Assets the window loads: the component icon catalog, Limen's own mark,
//! and the providers' logos, from lobe-icons (MIT, see
//! `assets/logos/LICENSE`).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The asset path of the Limen mark, drawn like any icon in the text color.
pub const MARK: &str = "limen/mark.svg";

/// The providers' logos, by asset path.
const LOGOS: [(&str, &[u8]); 9] = [
    (
        "limen/logos/openai.svg",
        include_bytes!("../assets/logos/openai.svg"),
    ),
    (
        "limen/logos/anthropic.svg",
        include_bytes!("../assets/logos/anthropic.svg"),
    ),
    (
        "limen/logos/gemini-color.svg",
        include_bytes!("../assets/logos/gemini-color.svg"),
    ),
    (
        "limen/logos/deepseek-color.svg",
        include_bytes!("../assets/logos/deepseek-color.svg"),
    ),
    (
        "limen/logos/mistral-color.svg",
        include_bytes!("../assets/logos/mistral-color.svg"),
    ),
    (
        "limen/logos/groq.svg",
        include_bytes!("../assets/logos/groq.svg"),
    ),
    (
        "limen/logos/openrouter.svg",
        include_bytes!("../assets/logos/openrouter.svg"),
    ),
    (
        "limen/logos/xai.svg",
        include_bytes!("../assets/logos/xai.svg"),
    ),
    (
        "limen/logos/githubcopilot.svg",
        include_bytes!("../assets/logos/githubcopilot.svg"),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            MARK => Ok(Some(Cow::Borrowed(include_bytes!("../assets/mark.svg")))),
            other => match LOGOS.iter().find(|(logo, _)| *logo == other) {
                Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
                None => gpui_kit::assets::AllAssets.load(other),
            },
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::AllAssets.list(path).map(|listed| {
            listed
                .into_iter()
                .chain(
                    std::iter::once(MARK)
                        .chain(LOGOS.map(|(logo, _)| logo))
                        .filter(|own| own.starts_with(path))
                        .map(SharedString::from),
                )
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource;

    use super::{Assets, LOGOS, MARK};

    #[test]
    fn limen_s_own_assets_are_listed_where_they_load() {
        let listed = Assets.list("limen/").unwrap();
        let loaded = listed
            .iter()
            .map(|path| Assets.load(path).unwrap().is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            (listed.len(), loaded.iter().all(|loads| *loads)),
            (1 + LOGOS.len(), true)
        );
        assert!(listed.contains(&gpui_kit::SharedString::from(MARK)));
    }
}
