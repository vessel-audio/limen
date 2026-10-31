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
    fn application_translations_resolve_at_runtime() {
        rust_i18n::set_locale("en");
        assert_eq!(rust_i18n::t!("status.idle"), "Idle");
        rust_i18n::set_locale("zh-CN");
        assert_eq!(rust_i18n::t!("status.idle"), "空闲");
    }

    #[test]
    fn every_translation_key_has_both_languages() {
        let keys = |source: &str| {
            source
                .lines()
                .filter_map(|line| line.split_once(':').map(|(key, _)| key.to_string()))
                .collect::<std::collections::BTreeSet<_>>()
        };
        let en = keys(include_str!("../locales/en.yml"));
        let zh = keys(include_str!("../locales/zh-CN.yml"));
        assert!(!en.is_empty());
        assert_eq!(en, zh);
    }
}
