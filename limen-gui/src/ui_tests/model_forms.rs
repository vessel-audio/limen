//! The dialogs of the provider cards: what each form sends to be saved and
//! how it shows the bridge's reply. What lands in the config and key files,
//! and the requests a daemon receives, are the bridge's: the Linux desktop
//! suite keeps one custom server saved through a daemon onto disk.

use std::collections::BTreeMap;

use gpui_kit::{ScrollDelta, TestAppContext, point, px};
use limen_core::{
    ApiKey, ChatApi, ConfigChange, Format, KeyChange, ModelRole, Polishing, Provider,
    ProviderOptions, Service,
};
use rust_i18n::t;

use super::model_support::{
    Settings, choose, loads, open_providers, save_of, save_replied, type_model,
};
use super::{Daemon, Ui};

/// A key as the form sends it once typed.
fn typed(key: &str) -> KeyChange {
    KeyChange::Set(ApiKey::new(key.to_string()))
}

/// The text a select shows as its value.
fn value(ui: &mut Ui, id: &str) -> Option<String> {
    ui.find(id).value().map(str::to_string)
}

/// A custom whisper.cpp server, its key, and a typed model are saved from
/// the dialog together; once the bridge applies them the dialog closes, a
/// toast says so, and the page reads its settings anew.
#[gpui_kit::test]
fn a_custom_asr_server_is_saved_from_its_dialog(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    choose(&mut ui, "asr-format", &["down"]);
    ui.retype("asr-url", "http://10.0.0.2:9000/inference");
    ui.click("asr-key");
    ui.input("sk-asr");
    type_model(&mut ui, "asr", "Qwen/Qwen3-ASR-1.7B");
    let chosen = (value(&mut ui, "asr-format"), value(&mut ui, "asr-model"));
    ui.click("asr-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Asr, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (
            chosen,
            provider,
            key,
            ui.label("toast"),
            ui.try_find("asr-key").is_some(),
            loads(&ui.sent())
                .into_iter()
                .map(|(role, _)| role)
                .collect::<Vec<_>>(),
        ),
        (
            (
                Some("whisper.cpp".to_string()),
                Some("Qwen/Qwen3-ASR-1.7B".to_string())
            ),
            Provider::Asr {
                service: Service::Custom,
                format: Format::WhisperCpp,
                url: "http://10.0.0.2:9000/inference".to_string(),
                model: "Qwen/Qwen3-ASR-1.7B".to_string(),
                options: BTreeMap::new(),
            },
            typed("sk-asr"),
            t!("toast.asr_applied").into(),
            false,
            vec![ModelRole::Asr],
        )
    );
}

/// A vendor is reached at its own URL unless another is typed, as for a
/// gateway; saving it names the vendor and keeps that URL in its own table.
#[gpui_kit::test]
fn a_vendor_is_chosen_from_its_card(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-groq");
    let own_url = value(&mut ui, "asr-url");
    ui.retype("asr-url", "https://gateway.example/groq/v1");
    ui.click("asr-key");
    ui.input("sk-groq");
    type_model(&mut ui, "asr", "whisper-large-v3");
    ui.click("asr-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Asr, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (own_url, provider, key, ui.label("toast")),
        (
            Service::Groq.default_url().map(str::to_string),
            Provider::Asr {
                service: Service::Groq,
                format: Format::OpenAiCompatible,
                url: String::new(),
                model: "whisper-large-v3".to_string(),
                options: BTreeMap::from([(
                    Service::Groq,
                    ProviderOptions {
                        url: Some("https://gateway.example/groq/v1".to_string()),
                        ..ProviderOptions::default()
                    }
                )]),
            },
            typed("sk-groq"),
            t!("toast.asr_applied").into(),
        )
    );
}

/// When the bridge refuses the server, the dialog stays open with the reason
/// and the edits, ready to save again, and no toast says anything was
/// applied; the page reads its settings anew, as the refusal may have left
/// something written.
#[gpui_kit::test]
fn an_invalid_asr_server_is_reported(cx: &mut TestAppContext) {
    const REFUSED: &str = "provider.url must be an http(s):// URL with a host";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    ui.retype("asr-url", "ftp://10.0.0.2");
    type_model(&mut ui, "asr", "typed-model");
    ui.click("asr-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Asr, id, Err(REFUSED.to_string()));
    assert_eq!(
        (
            provider,
            key,
            ui.label("asr-error"),
            ui.find("asr-save").disabled(),
            value(&mut ui, "asr-url"),
            ui.try_find("toast").is_some(),
            loads(&ui.sent()).len(),
        ),
        (
            Provider::Asr {
                service: Service::Custom,
                format: Format::OpenAiCompatible,
                url: "ftp://10.0.0.2".to_string(),
                model: "typed-model".to_string(),
                options: BTreeMap::new(),
            },
            KeyChange::Keep,
            REFUSED.to_string(),
            None,
            Some("ftp://10.0.0.2".to_string()),
            false,
            1,
        )
    );
}

/// A saved key can be removed: the dialog says the key is removed once
/// saved, and the save asks the bridge to clear it.
#[gpui_kit::test]
fn a_saved_key_is_removed(cx: &mut TestAppContext) {
    let settings = Settings::read(
        "[provider]\nurl = \"\"\n\n[polish]\nenabled = true\nservice = \"custom\"\n\
         url = \"http://127.0.0.1:9/v1\"\nmodel = \"m\"\n",
        "asr = \"sk-asr\"\npolish = \"sk-polish\"\n",
    );
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &settings);
    ui.click("polish-provider-custom");
    let saved = (
        ui.find("polish-key").placeholder().map(str::to_string),
        ui.try_find("polish-key-remove").is_some(),
    );
    ui.click("polish-key-remove");
    let removed = (
        ui.find("polish-key").placeholder().map(str::to_string),
        ui.try_find("polish-key-remove").is_some(),
    );
    ui.click("polish-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Polish, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (saved, removed, provider, key, ui.label("toast")),
        (
            (Some(t!("models.key_saved").into()), true),
            (Some(t!("models.key_removed").into()), false),
            Provider::Polish {
                polishing: Polishing::On,
                service: Service::Custom,
                format: Format::OpenAiCompatible,
                url: "http://127.0.0.1:9/v1".to_string(),
                model: "m".to_string(),
                options: BTreeMap::new(),
            },
            KeyChange::Clear,
            t!("toast.polish_applied").into(),
        )
    );
}

/// Credentials belong in the key field, not in a provider URL: the URL is
/// sent as typed for the bridge to judge, and its refusal shows in the
/// dialog, which stays open.
#[gpui_kit::test]
fn a_url_credential_is_rejected(cx: &mut TestAppContext) {
    const REFUSED: &str = "polish.url allows only non-secret query parameters such as \
                           api-version; use an API key instead";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("polishing");
    ui.click("polish-provider-custom");
    ui.retype("polish-url", "https://ai.example/v1?api_key=do-not-save");
    ui.click("polish-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Polish, id, Err(REFUSED.to_string()));
    assert_eq!(
        (
            provider,
            key,
            ui.label("polish-error"),
            ui.try_find("toast").is_some()
        ),
        (
            Provider::Polish {
                polishing: Polishing::On,
                service: Service::Custom,
                format: Format::OpenAiCompatible,
                url: "https://ai.example/v1?api_key=do-not-save".to_string(),
                model: String::new(),
                options: BTreeMap::new(),
            },
            KeyChange::Keep,
            REFUSED.to_string(),
            false,
        )
    );
}

/// Editing while a save is in flight keeps the newer text and the dialog
/// open once the save is applied, so the edits can be saved next.
#[gpui_kit::test]
fn edits_made_while_saving_survive(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    ui.retype("asr-url", "http://a/v1");
    type_model(&mut ui, "asr", "typed-model");
    ui.click("asr-save");
    let (first, provider_a, _) = save_of(&ui.sent());
    let in_flight = ui.find("asr-save").disabled();
    ui.retype("asr-url", "http://b/v1");
    save_replied(&mut ui, ModelRole::Asr, first, Ok(ConfigChange::Applied));
    let applied = (
        ui.label("toast"),
        ui.try_find("asr-save").is_some(),
        ui.find("asr-save").disabled(),
        value(&mut ui, "asr-url"),
    );
    ui.click("asr-save");
    let (second, provider_b, _) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Asr, second, Ok(ConfigChange::Applied));
    let urls = [provider_a, provider_b].map(|provider| match provider {
        Provider::Asr { url, .. } | Provider::Polish { url, .. } => url,
    });
    assert_eq!(
        (
            urls,
            first != second,
            in_flight,
            applied,
            ui.try_find("asr-save").is_some()
        ),
        (
            ["http://a/v1".to_string(), "http://b/v1".to_string()],
            true,
            Some(true),
            (
                t!("toast.asr_applied").into(),
                true,
                None,
                Some("http://b/v1".to_string())
            ),
            false,
        )
    );
}

/// Only OpenAI's APIs offer a choice of chat API, and one made other than the
/// provider's own is saved under its table.
#[gpui_kit::test]
fn openai_polishing_chooses_its_chat_api(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("polishing");
    ui.click("polish-provider-anthropic");
    let anthropic = ui.try_find("polish-chat-api").is_some();
    ui.click("polish-cancel");
    ui.click("polish-provider-openai");
    let openai = ui.try_find("polish-chat-api").is_some();
    choose(&mut ui, "polish-chat-api", &["up"]);
    ui.click("polish-key");
    ui.input("sk-openai");
    type_model(&mut ui, "polish", "typed-model");
    ui.click("polish-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Polish, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (anthropic, openai, provider, key, ui.label("toast")),
        (
            false,
            true,
            Provider::Polish {
                polishing: Polishing::On,
                service: Service::OpenAi,
                format: Format::OpenAiCompatible,
                url: String::new(),
                model: "typed-model".to_string(),
                options: BTreeMap::from([(
                    Service::OpenAi,
                    ProviderOptions {
                        chat_api: Some(ChatApi::ChatCompletions),
                        ..ProviderOptions::default()
                    }
                )]),
            },
            typed("sk-openai"),
            t!("toast.polish_applied").into(),
        )
    );
}

/// A provider's advanced settings, unfolded and scrolled to in its dialog,
/// are saved under its own table.
#[gpui_kit::test]
fn advanced_settings_belong_to_their_provider(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    ui.retype("asr-url", "http://10.0.0.2:9000/v1");
    type_model(&mut ui, "asr", "typed-model");
    ui.click("asr-advanced");
    ui.retype("asr-key-header", "api-key");
    ui.retype("asr-headers", "X-Org: limen");
    // The advanced settings run past the dialog's height: scroll to the rest
    // over the toggle, which keeps the wheel for the fields' scrolling.
    ui.scroll(
        "asr-advanced",
        ScrollDelta::Pixels(point(px(0.), px(-1000.))),
    );
    ui.retype("asr-timeout", "8000");
    ui.click("asr-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Asr, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (provider, key, ui.label("toast")),
        (
            Provider::Asr {
                service: Service::Custom,
                format: Format::OpenAiCompatible,
                url: "http://10.0.0.2:9000/v1".to_string(),
                model: "typed-model".to_string(),
                options: BTreeMap::from([(
                    Service::Custom,
                    ProviderOptions {
                        key_header: Some("api-key".to_string()),
                        timeout_ms: Some(8000),
                        headers: BTreeMap::from([("X-Org".to_string(), "limen".to_string())]),
                        ..ProviderOptions::default()
                    }
                )]),
            },
            KeyChange::Keep,
            t!("toast.asr_applied").into(),
        )
    );
}
