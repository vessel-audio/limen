//! The model provider pages under Models in the sidebar: one for the ASR
//! provider that transcribes, one for the provider that polishes.

use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::Input;
use gpui_kit::component::select::Select;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt};
use gpui_kit::*;
use limen_core::{KeyState, ModelRole, Polishing};

use crate::a11y;
use crate::app::AppView;
use crate::settings::{Segment, segmented};

mod choices;
mod form;

pub use form::{ModelForms, follow};

use form::{Address, save};

pub fn models(view: &AppView, role: ModelRole, cx: &mut Context<AppView>) -> Div {
    let (form, prefix, title) = match role {
        ModelRole::Asr => (&view.models.asr, "asr", rust_i18n::t!("models.asr_title")),
        ModelRole::Polish => (
            &view.models.polish,
            "polish",
            rust_i18n::t!("models.polish_title"),
        ),
    };
    let address = match &form.address {
        Address::Server { server, route } => vec![(
            rust_i18n::t!("models.server").to_string(),
            div()
                .h_flex()
                .gap_2()
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(server)
                            .accessibility_id("asr-server")
                            .aria_label(rust_i18n::t!("models.server").to_string()),
                    ),
                )
                .child(
                    div().flex_shrink_0().w(px(220.)).child(
                        Select::new(route)
                            .id("asr-route")
                            .accessibility_label(rust_i18n::t!("models.route").to_string())
                            // Names a hand-edited route the list does not offer.
                            .placeholder(
                                form.unlisted_route
                                    .clone()
                                    .unwrap_or_else(|| rust_i18n::t!("models.route").to_string()),
                            ),
                    ),
                )
                .into_any_element(),
        )],
        Address::Url { polishing, url } => vec![
            (
                rust_i18n::t!("models.polishing").to_string(),
                // A row of its own, so the track keeps the width of its options.
                div()
                    .h_flex()
                    .child(segmented(
                        "polishing",
                        rust_i18n::t!("models.polishing").to_string(),
                        rems(4.),
                        vec![
                            Segment {
                                id: "polishing-off".into(),
                                label: rust_i18n::t!("settings.off").to_string(),
                                value: Polishing::Off,
                            },
                            Segment {
                                id: "polishing-on".into(),
                                label: rust_i18n::t!("settings.on").to_string(),
                                value: Polishing::On,
                            },
                        ],
                        *polishing,
                        |this, polishing, _, _| {
                            let form = this.models.form(ModelRole::Polish);
                            if let Address::Url {
                                polishing: shown, ..
                            } = &mut form.address
                            {
                                *shown = polishing;
                            }
                            form.edited();
                        },
                        cx,
                    ))
                    .into_any_element(),
            ),
            (
                rust_i18n::t!("models.url").to_string(),
                Input::new(url)
                    .accessibility_id("polish-url")
                    .aria_label(rust_i18n::t!("models.url").to_string())
                    .into_any_element(),
            ),
        ],
    };
    let saved_key = matches!(form.loaded, Some((_, KeyState::Saved))) && !form.clear_key;
    let key = div()
        .h_flex()
        .gap_2()
        .child(
            div().flex_1().min_w_0().child(
                Input::new(&form.key)
                    .mask_toggle()
                    .accessibility_id(format!("{prefix}-key"))
                    .aria_label(rust_i18n::t!("models.key").to_string()),
            ),
        )
        .children(saved_key.then(|| {
            a11y::button(format!("{prefix}-key-remove"))
                .ghost()
                .label(rust_i18n::t!("models.remove_key").to_string())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let form = this.models.form(role);
                    form.clear_key = true;
                    form.edited();
                    form.key.update(cx, |input, cx| {
                        input.set_placeholder(
                            rust_i18n::t!("models.key_removed").to_string(),
                            window,
                            cx,
                        )
                    });
                }))
        }));
    let model = Select::new(&form.model)
        .id(SharedString::from(format!("{prefix}-model")))
        .accessibility_label(rust_i18n::t!("models.model").to_string())
        .placeholder(rust_i18n::t!("models.model").to_string())
        .search_placeholder(rust_i18n::t!("models.search").to_string());
    let rows = address.into_iter().chain([
        (
            rust_i18n::t!("models.key").to_string(),
            key.into_any_element(),
        ),
        (
            rust_i18n::t!("models.model").to_string(),
            model.into_any_element(),
        ),
    ]);
    div()
        .v_flex()
        .w_full()
        .max_w(px(720.))
        .mx_auto()
        .gap_5()
        .p_8()
        .child(super::title(title))
        .child(
            div()
                .v_flex()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .children(rows.enumerate().map(|(index, (label, control))| {
                    let row = div()
                        .h_flex()
                        .items_center()
                        .gap_4()
                        .px_5()
                        .py_3()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(96.))
                                .text_sm()
                                .font_medium()
                                .child(label),
                        )
                        .child(div().flex_1().min_w_0().child(control));
                    match index {
                        0 => row,
                        _ => row.border_t_1(),
                    }
                })),
        )
        .child(
            div()
                .h_flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .children(form.error.as_ref().map(|error| {
                            a11y::alert(
                                match role {
                                    ModelRole::Asr => "asr-error",
                                    ModelRole::Polish => "polish-error",
                                },
                                error.clone(),
                            )
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone())
                        })),
                )
                .child(
                    a11y::button(format!("{prefix}-save"))
                        .primary()
                        .label(rust_i18n::t!("models.save").to_string())
                        .flex_shrink_0()
                        .disabled(!form.touched || form.saving.is_some() || form.loaded.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| save(this, role, cx))),
                ),
        )
}
