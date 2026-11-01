//! The ASR endpoint the daemon transcribes with: one row of the dictation
//! control strip, with the error below it when a change fails.

use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, input::Input};
use gpui_kit::*;

use crate::a11y;
use crate::app::AppView;
use crate::bridge::BridgeCommand;

pub fn provider(view: &AppView, cx: &mut Context<AppView>, window: &mut Window) -> Div {
    // Follow the daemon until the user edits the field, but only when they
    // differ: setting a value resets the field's cursor and selection, and
    // this runs on every frame.
    if let Some(url) = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.state.config.provider_url.clone())
        && !view.provider_touched
        && view.provider_input.read(cx).value() != url
    {
        view.provider_input
            .update(cx, |input, cx| input.set_value(url, window, cx));
    }
    let title = rust_i18n::t!("provider.title").to_string();
    div()
        .v_flex()
        .gap_1()
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(title.clone()),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&view.provider_input)
                            .small()
                            .accessibility_id("provider-url")
                            .aria_label(title),
                    ),
                )
                .child(
                    a11y::button("provider-apply")
                        .small()
                        .outline()
                        .label(rust_i18n::t!("action.apply").to_string())
                        .flex_shrink_0()
                        .disabled(!view.provider_touched)
                        .on_click(cx.listener(|this, _, _, cx| apply_provider_url(this, cx))),
                ),
        )
        .children(view.provider_error.as_ref().map(|error| {
            a11y::alert("provider-error", error.clone())
                .text_xs()
                .text_color(cx.theme().danger)
                .child(error.clone())
        }))
}

/// Hand an edited ASR endpoint to the bridge; the core validates it.
pub fn apply_provider_url(view: &mut AppView, cx: &mut Context<AppView>) {
    if view.provider_touched {
        let url = view.provider_input.read(cx).value().trim().to_string();
        let _ = view.commands.try_send(BridgeCommand::SetProviderUrl(url));
    }
}
