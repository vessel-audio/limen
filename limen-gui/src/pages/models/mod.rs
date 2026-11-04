//! The Providers page under Models in the sidebar: a section for the ASR
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

pub(super) use choices::{logo, service_label};
pub use form::{ModelForms, follow};

use form::set_polishing;

/// The Providers page: a section for the ASR provider, then one for the
/// polishing provider, whose switch heads it.
pub fn providers(view: &AppView, window: &Window, cx: &mut Context<AppView>) -> Div {
    let scroll = &view.providers_scroll;
    // The window less the sidebar, the main area's margin and border, and
    // the page's padding.
    let width = window.viewport_size().width
        - view.sidebar_mode.width(view.sidebar_width)
        - px(8. + 2. + 64.);
    let content = div()
        .v_flex()
        .w_full()
        .gap_10()
        .p_8()
        .on_scroll(scroll.on_scroll())
        .child(super::title(rust_i18n::t!("nav.providers")))
        .child(section(view, ModelRole::Asr, width, cx))
        .child(section(view, ModelRole::Polish, width, cx));
    // Taller than the window, as with the providers and an error shown, the
    // page scrolls rather than running under the footer.
    scroll
        .container(
            div()
                .id("providers-scroll")
                .accessibility_id("providers-scroll")
                .child(content),
            ScrollbarMode::Scrolling,
        )
        .flex_1()
}

/// One role's section: its heading, its provider cards, and its error.
fn section(view: &AppView, role: ModelRole, width: Pixels, cx: &mut Context<AppView>) -> Div {
    let (form, heading, heading_id, error_id) = match role {
        ModelRole::Asr => (
            &view.models.asr,
            rust_i18n::t!("models.asr_title"),
            "asr-heading",
            "asr-error",
        ),
        ModelRole::Polish => (
            &view.models.polish,
            rust_i18n::t!("models.polish_title"),
            "polish-heading",
            "polish-error",
        ),
    };
    let switch = match role {
        ModelRole::Asr => None,
        ModelRole::Polish => Some(
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
    };
    // An open dialog shows its own error.
    let error = form.error.clone().filter(|_| !form.open);
    div()
        .v_flex()
        .gap_5()
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .id(heading_id)
                        .accessibility_id(heading_id)
                        .role(Role::Heading)
                        .aria_label(heading.to_string())
                        .flex_1()
                        .text_lg()
                        .font_family("serif")
                        .child(heading.to_string()),
                )
                .children(switch),
        )
        // Polishing off needs no provider.
        .children(match (role, form.polishing) {
            (ModelRole::Asr, _) | (ModelRole::Polish, Polishing::On) => {
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
        }))
}
