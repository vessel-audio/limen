//! The model provider pages under Models in the sidebar: one for the ASR
//! provider that transcribes, one for the provider that polishes. Each
//! offers its services as cards; a card opens the dialog that sets it up.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt};
use gpui_kit::*;
use limen_core::{ModelRole, Polishing};

use crate::a11y;
use crate::app::AppView;

mod cards;
mod choices;
mod dialog;
mod form;

pub use form::{ModelForms, follow};

use form::set_polishing;

pub fn models(view: &AppView, role: ModelRole, cx: &mut Context<AppView>) -> Div {
    let (form, title, error_id) = match role {
        ModelRole::Asr => (
            &view.models.asr,
            rust_i18n::t!("models.asr_title"),
            "asr-error",
        ),
        ModelRole::Polish => (
            &view.models.polish,
            rust_i18n::t!("models.polish_title"),
            "polish-error",
        ),
    };
    let switch = match role {
        ModelRole::Asr => None,
        ModelRole::Polish => Some(
            div()
                .h_flex()
                .items_center()
                .gap_4()
                .px_5()
                .py_3()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_medium()
                        .child(rust_i18n::t!("models.polishing").to_string()),
                )
                .child(
                    Switch::new("polishing")
                        .checked(form.polishing == Polishing::On)
                        .accessibility_label(rust_i18n::t!("models.polishing").to_string())
                        .disabled(form.loaded.is_none() || form.saving.is_some())
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            set_polishing(
                                this,
                                match on {
                                    true => Polishing::On,
                                    false => Polishing::Off,
                                },
                            );
                            cx.notify();
                        })),
                ),
        ),
    };
    // An open dialog shows its own error.
    let error = form.error.clone().filter(|_| !form.open);
    div()
        .v_flex()
        .w_full()
        .max_w(px(720.))
        .mx_auto()
        .gap_5()
        .p_8()
        .child(super::title(title))
        .children(switch)
        .child(cards::grid(view, role, cx))
        .children(error.map(|error| {
            a11y::alert(error_id, error.clone())
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .with_animation(
                    SharedString::from(format!("{error_id}-{error}")),
                    crate::motion::enter(),
                    |alert, t| alert.opacity(t),
                )
        }))
}
