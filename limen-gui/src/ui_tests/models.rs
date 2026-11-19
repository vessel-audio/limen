//! The Providers page under Models in the sidebar: how it opens and asks for
//! its settings, which provider leads, the polishing switch, a subscription's
//! account row, and an error shown on the page. The dialogs' forms are in
//! `model_forms` and `model_discovery`. The Linux desktop suite keeps one
//! custom server saved through a daemon onto disk.

use std::collections::BTreeMap;

use gpui_kit::{ScrollDelta, TestAppContext, point, px};
use limen_core::{
    ConfigChange, Format, KeyChange, ModelRole, Polishing, Provider, Service, Subscription,
};
use rust_i18n::t;

use super::model_support::{Settings, loads, open_providers, save_of, save_replied, type_model};
use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent, ModelRequest};

/// The Models button on the dictation page opens the Providers page, which
/// asks for each role's settings.
#[gpui_kit::test]
fn the_dictation_button_opens_the_providers_page(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("open-models");
    assert_eq!(
        (
            ui.find("providers-page").selected(),
            ui.label("current-page"),
            loads(&ui.sent())
        ),
        (
            Some(true),
            t!("nav.providers").into(),
            vec![(ModelRole::Asr, 1), (ModelRole::Polish, 1)]
        )
    );
}

/// With polishing off the page offers no providers; the switch shows them
/// and asks for nothing to be saved until one is set up.
#[gpui_kit::test]
fn the_switch_shows_the_providers(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    let shown = |ui: &mut Ui| {
        (
            ui.find("polishing").checked(),
            ui.try_find("polish-provider-custom").is_some(),
        )
    };
    let off = shown(&mut ui);
    ui.click("polishing");
    assert_eq!(
        (off, shown(&mut ui), ui.sent()),
        ((Some(false), false), (Some(true), true), Vec::new())
    );
}

/// The configured provider leads the page; the others fold under a toggle,
/// and fold again once the page is left and shown again, when it reads its
/// settings anew.
#[gpui_kit::test]
fn the_configured_provider_leads(cx: &mut TestAppContext) {
    let settings = Settings::read(
        "[provider]\nurl = \"\"\n\n[polish]\nenabled = true\nservice = \"groq\"\nmodel = \"m\"\n",
        "",
    );
    let mut ui = Ui::connected(cx, Daemon::default());
    let shown = |ui: &mut Ui| {
        [
            "polish-others",
            "polish-provider-groq",
            "polish-provider-custom",
        ]
        .map(|id| ui.try_find(id).is_some())
    };
    let first = open_providers(&mut ui, &settings);
    let folded = shown(&mut ui);
    ui.click("polish-others");
    let unfolded = shown(&mut ui);
    ui.click("dictation-page");
    let second = open_providers(&mut ui, &settings);
    assert_eq!(
        (
            loads(&first),
            folded,
            unfolded,
            loads(&second),
            shown(&mut ui)
        ),
        (
            vec![(ModelRole::Asr, 1), (ModelRole::Polish, 1)],
            [true, true, false],
            [true, true, true],
            vec![(ModelRole::Asr, 2), (ModelRole::Polish, 2)],
            [true, true, false],
        )
    );
}

/// A subscription is signed in to rather than given a key: its dialog asks
/// whether a sign-in is kept, then asks for no key or URL, says it is not
/// signed in yet, and saves without a key.
#[gpui_kit::test]
fn a_subscription_is_signed_in_to_not_given_a_key(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("polishing");
    ui.click("polish-provider-chatgpt");
    let asked = ui
        .sent()
        .contains(&BridgeCommand::Model(ModelRequest::SignedIn {
            subscription: Subscription::ChatGpt,
        }));
    ui.receive(BridgeEvent::SignedIn {
        subscription: Subscription::ChatGpt,
        signed_in: false,
    });
    let account = (
        ui.label("polish-account"),
        ui.try_find("polish-sign-in").is_some(),
        ui.try_find("polish-key").is_some(),
        ui.try_find("polish-url").is_some(),
    );
    type_model(&mut ui, "polish", "typed-model");
    ui.click("polish-save");
    let (id, provider, key) = save_of(&ui.sent());
    save_replied(&mut ui, ModelRole::Polish, id, Ok(ConfigChange::Applied));
    assert_eq!(
        (
            asked,
            account,
            provider,
            key,
            ui.label("toast"),
            ui.try_find("polish-save").is_some()
        ),
        (
            true,
            (t!("models.signed_out").into(), true, false, false),
            Provider::Polish {
                polishing: Polishing::On,
                service: Service::ChatGpt,
                format: Format::OpenAiCompatible,
                url: String::new(),
                model: "typed-model".to_string(),
                options: BTreeMap::new(),
            },
            KeyChange::Keep,
            t!("toast.polish_applied").into(),
            false,
        )
    );
}

/// An error under all the polishing providers is out of sight at first; the
/// wheel scrolls it into view within the page rather than leaving it under the
/// window's edge.
#[gpui_kit::test]
fn a_page_error_below_the_providers_scrolls_into_view(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    open_providers(&mut ui, &Settings::default());
    ui.click("polishing");
    ui.click("polish-provider-custom");
    ui.retype("polish-url", "ftp://10.0.0.2");
    type_model(&mut ui, "polish", "typed-model");
    ui.click("polish-save");
    let (id, ..) = save_of(&ui.sent());
    save_replied(
        &mut ui,
        ModelRole::Polish,
        id,
        Err("polish.url must be an http(s):// URL with a host".to_string()),
    );
    // Closed, the dialog leaves its error on the page.
    ui.click("polish-cancel");
    let within = |ui: &mut Ui| {
        let (error, viewport) = (
            ui.find("polish-error").bounds(),
            ui.find("providers-viewport").bounds(),
        );
        error.top() >= viewport.top() && error.bottom() <= viewport.bottom()
    };
    let before = within(&mut ui);
    ui.scroll(
        "providers-viewport",
        ScrollDelta::Pixels(point(px(0.), px(-5000.))),
    );
    assert_eq!(
        (before, within(&mut ui)),
        (false, true),
        "the error is {:?} in the page {:?}",
        ui.find("polish-error").bounds(),
        ui.find("providers-viewport").bounds()
    );
}
