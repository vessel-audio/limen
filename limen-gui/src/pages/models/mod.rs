//! The model provider pages under Models in the sidebar: one for the ASR
//! provider that transcribes, one for the provider that polishes. Each
//! offers its services as cards; a card opens the dialog that sets it up.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, StyledExt, scroll::ScrollbarMode};
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

pub fn models(view: &AppView, role: ModelRole, window: &Window, cx: &mut Context<AppView>) -> Div {
    let (form, title, error_id, scroll, area) = match role {
        ModelRole::Asr => (
            &view.models.asr,
            rust_i18n::t!("models.asr_title"),
            "asr-error",
            &view.asr_scroll,
            "asr-scroll",
        ),
        ModelRole::Polish => (
            &view.models.polish,
            rust_i18n::t!("models.polish_title"),
            "polish-error",
            &view.polish_scroll,
            "polish-scroll",
        ),
    };
    let switch = match role {
        ModelRole::Asr => None,
        ModelRole::Polish => Some(
            div()
                .h_flex()
                .items_center()
                .gap_4()
                .py_3()
                .rounded_xl()
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
    let content = div()
        .v_flex()
        .w_full()
        .gap_5()
        .p_8()
        .on_scroll(scroll.on_scroll())
        .child(super::title(title))
        .children(switch)
        // Polishing off needs no provider.
        .children(match (role, form.polishing) {
            (ModelRole::Asr, _) | (ModelRole::Polish, Polishing::On) => {
                // The window less the sidebar, the main area's margin and
                // border, and the page's padding.
                let width = window.viewport_size().width
                    - view.sidebar_mode.width(view.sidebar_width)
                    - px(8. + 2. + 64.);
                Some(cards::grid(view, role, width, cx))
            }
            (ModelRole::Polish, Polishing::Off) => None,
        })
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
        }));
    // Taller than the window, as with the providers and an error shown, the
    // page scrolls rather than running under the footer.
    scroll
        .container(
            div().id(area).accessibility_id(area).child(content),
            ScrollbarMode::Scrolling,
        )
        .flex_1()
}
