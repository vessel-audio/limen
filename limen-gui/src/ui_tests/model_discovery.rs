//! The dialogs' model selects: asking the server the fields name for its
//! models as they are typed and on request, choosing the only one a server
//! lists, and trying a model before saving it. The servers' answers are the
//! bridge's replies; the dialog is judged by what it sends and shows.

use std::collections::BTreeMap;

use gpui_kit::TestAppContext;
use limen_core::{
    ApiKey, ConfigChange, Format, KeyChange, ModelRole, Polishing, Provider, Service,
};
use rust_i18n::t;

use super::model_support::{
    Settings, after_discovery_wait, check_of, choose, last_list, lists, open_providers, save_of,
    save_replied, saves,
};
use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent};

/// The text a select shows as its value.
fn value(ui: &mut Ui, id: &str) -> Option<String> {
    ui.find(id).value().map(str::to_string)
}

/// The custom server at `url` as the fields name it for `role`, with
/// polishing on for the polishing role.
fn custom(role: ModelRole, url: &str, model: &str) -> Provider {
    let (url, model) = (url.to_string(), model.to_string());
    match role {
        ModelRole::Asr => Provider::Asr {
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url,
            model,
            options: BTreeMap::new(),
        },
        ModelRole::Polish => Provider::Polish {
            polishing: Polishing::On,
            service: Service::Custom,
            format: Format::OpenAiCompatible,
            url,
            model,
            options: BTreeMap::new(),
        },
    }
}

/// With the switch on, a local polishing server is set up in its dialog,
/// asking for its models with the typed key once the fields settle; saving
/// the model chosen from the list turns polishing on.
#[gpui_kit::test]
fn polishing_is_set_up_with_a_local_server(cx: &mut TestAppContext) {
    let (url, key) = ("http://127.0.0.1:8080/v1", "sk-polish");
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("polishing");
    ui.click("polish-provider-custom");
    ui.retype("polish-url", url);
    ui.click("polish-key");
    ui.input(key);
    let (role, request, asked, asked_key) = last_list(&after_discovery_wait(&mut ui));
    ui.receive(BridgeEvent::Models {
        role,
        request,
        models: Ok(vec!["listed-a".to_string(), "listed-b".to_string()]),
    });
    choose(&mut ui, "polish-model", &["down", "down"]);
    let chosen = value(&mut ui, "polish-model");
    ui.click("polish-save");
    let (id, provider, saved_key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Polish, id, Ok(ConfigChange::Applied));
    let typed = KeyChange::Set(ApiKey::new(key.to_string()));
    assert_eq!(
        (
            role,
            asked,
            asked_key,
            chosen,
            provider,
            saved_key,
            ui.label("toast"),
            ui.try_find("polish-key").is_some(),
        ),
        (
            ModelRole::Polish,
            custom(ModelRole::Polish, url, ""),
            typed.clone(),
            Some("listed-b".to_string()),
            custom(ModelRole::Polish, url, "listed-b"),
            typed,
            t!("toast.polish_applied").into(),
            false,
        )
    );
}

/// The model is not filled in until there is a server to ask: nothing is
/// asked while the address is empty, and typing one that lists a single
/// model chooses it, which saving keeps.
#[gpui_kit::test]
fn the_model_is_detected_from_the_typed_server(cx: &mut TestAppContext) {
    let url = "http://127.0.0.1:8000/v1";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    let before_a_server = ui.sent();
    ui.retype("asr-url", url);
    let (role, request, asked, asked_key) = last_list(&after_discovery_wait(&mut ui));
    ui.receive(BridgeEvent::Models {
        role,
        request,
        models: Ok(vec!["only-asr".to_string()]),
    });
    let chosen = value(&mut ui, "asr-model");
    ui.click("asr-save");
    let (_, provider, _) = save_of(&ui.sent());
    assert_eq!(
        (before_a_server, role, asked, asked_key, chosen, provider),
        (
            Vec::new(),
            ModelRole::Asr,
            custom(ModelRole::Asr, url, ""),
            KeyChange::Keep,
            Some("only-asr".to_string()),
            custom(ModelRole::Asr, url, "only-asr"),
        )
    );
}

/// Detect asks for the models at once, and says so when there is no server to
/// ask; the models it is answered with are offered by the select.
#[gpui_kit::test]
fn the_models_are_detected_on_request(cx: &mut TestAppContext) {
    let url = "http://127.0.0.1:8000/v1";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    ui.click("asr-detect");
    let no_server = (ui.label("asr-error"), ui.sent());
    ui.retype("asr-url", url);
    // What typing asked is no answer to Detect: only the press is read.
    ui.sent();
    ui.click("asr-detect");
    let sent = ui.sent();
    let (role, request, asked, _) = last_list(&sent);
    ui.receive(BridgeEvent::Models {
        role,
        request,
        models: Ok(vec!["listed-a".to_string(), "listed-b".to_string()]),
    });
    let answered = ui.try_find("asr-error").is_some();
    choose(&mut ui, "asr-model", &["down", "down"]);
    assert_eq!(
        (
            no_server,
            lists(&sent).len(),
            role,
            asked,
            answered,
            value(&mut ui, "asr-model")
        ),
        (
            (t!("models.enter_server").into(), Vec::new()),
            1,
            ModelRole::Asr,
            custom(ModelRole::Asr, url, ""),
            false,
            Some("listed-b".to_string()),
        )
    );
}

/// Try the model of the dialog `prefix` names once the server at `url`
/// lists only it, as the bridge's replies say: every request the dialog
/// sent, the model check among them, and the toast the answer brings.
fn tried(
    ui: &mut Ui,
    role: ModelRole,
    prefix: &str,
    url: &str,
) -> (Vec<BridgeCommand>, (ModelRole, Provider, KeyChange), String) {
    ui.retype(&format!("{prefix}-url"), url);
    let mut sent = after_discovery_wait(ui);
    let (_, request, ..) = last_list(&sent);
    ui.receive(BridgeEvent::Models {
        role,
        request,
        models: Ok(vec!["only-model".to_string()]),
    });
    ui.click(&format!("{prefix}-test"));
    sent.extend(ui.sent());
    let (role, id, provider, key) = check_of(&sent);
    ui.receive(BridgeEvent::ModelChecked {
        role,
        id,
        result: Ok(()),
    });
    (sent, (role, provider, key), ui.label("toast"))
}

/// Test asks the chosen model to do its job with the fields as typed, for
/// each role, and saves nothing.
#[gpui_kit::test]
fn a_model_is_tested_before_it_is_saved(cx: &mut TestAppContext) {
    let url = "http://127.0.0.1:8000/v1";
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("asr-provider-custom");
    let (asr_sent, asr, asr_toast) = tried(&mut ui, ModelRole::Asr, "asr", url);
    ui.click("asr-cancel");
    ui.click("polishing");
    ui.click("polish-provider-custom");
    let (polish_sent, polish, polish_toast) = tried(&mut ui, ModelRole::Polish, "polish", url);
    let sent = [asr_sent, polish_sent, ui.sent()].concat();
    assert_eq!(
        (asr, asr_toast, polish, polish_toast, saves(&sent).len()),
        (
            (
                ModelRole::Asr,
                custom(ModelRole::Asr, url, "only-model"),
                KeyChange::Keep
            ),
            t!("toast.asr_model_works").into(),
            (
                ModelRole::Polish,
                custom(ModelRole::Polish, url, "only-model"),
                KeyChange::Keep
            ),
            t!("toast.polish_model_works").into(),
            0,
        )
    );
}
