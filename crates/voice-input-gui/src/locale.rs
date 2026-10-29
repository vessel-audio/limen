//! System locale selection and GUI translation initialization.

/// The two languages owned by the GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
}

impl Locale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    /// Detect the locale from an injectable environment lookup.
    pub fn detect(get: impl Fn(&str) -> Option<String>) -> Self {
        ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(get)
            .find(|value| !value.is_empty())
            .map_or(Self::En, |value| {
                match value.to_ascii_lowercase().starts_with("zh") {
                    true => Self::ZhCn,
                    false => Self::En,
                }
            })
    }

    /// Initialize both translation runtimes to the same language.
    pub fn initialize(self) {
        rust_i18n::set_locale(self.as_str());
        gpui_kit::component::set_locale(self.as_str());
    }
}

/// Select and initialize the system locale once at startup.
pub fn initialize() {
    Locale::detect(|name| std::env::var(name).ok()).initialize();
}

#[cfg(test)]
mod tests {
    use super::Locale;

    #[test]
    fn locale_precedence_and_chinese_fallbacks_are_deterministic() {
        let precedence = Locale::detect(|name| match name {
            "LANGUAGE" => Some(String::new()),
            "LC_ALL" => Some("zh_CN.UTF-8".to_string()),
            "LC_MESSAGES" => Some("en_US.UTF-8".to_string()),
            _ => None,
        });
        assert_eq!(precedence, Locale::ZhCn);
        for value in ["zh", "zh_TW", "zh-Hant"] {
            assert_eq!(Locale::detect(|_| Some(value.to_string())), Locale::ZhCn);
        }
        assert_eq!(
            Locale::detect(|name| match name {
                "LANGUAGE" => Some("en_GB.UTF-8".to_string()),
                "LC_ALL" => Some("zh_CN".to_string()),
                _ => None,
            }),
            Locale::En
        );
        assert_eq!(Locale::detect(|_| None), Locale::En);
    }

    #[test]
    fn every_translation_key_has_both_languages() {
        let source = include_str!("../locales/app.yml");
        let keys = |locale: &str| {
            let mut in_locale = false;
            source
                .lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    if trimmed.ends_with(':') && !trimmed.contains(' ') {
                        in_locale = trimmed == format!("{locale}:");
                        return None;
                    }
                    in_locale
                        .then(|| trimmed.split(':').next().map(str::to_string))
                        .flatten()
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        let en = keys("en");
        let zh = keys("zh-CN");
        assert!(!en.is_empty());
        assert_eq!(en, zh);
    }
}
