//! A subscription dialog's account row, in place of a key: whether a
//! sign-in is kept, and while signing in, what the user must do.

use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{ActiveTheme, StyledExt};
use gpui_kit::*;
use limen_core::{ModelRole, SignInStep};

use super::super::form::{Account, ProviderForm, cancel_sign_in, sign_in, sign_out};
use crate::a11y;
use crate::app::AppView;

/// The account row of `form`'s dialog, whose ids start with `prefix`.
pub(super) fn row(
    form: &ProviderForm,
    prefix: &str,
    entity: &Entity<AppView>,
    role: ModelRole,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let act = |id: &str, label: SharedString, action: fn(&mut AppView, ModelRole)| {
        let entity = entity.clone();
        a11y::button(format!("{prefix}-{id}"))
            .ghost()
            .label(label)
            .flex_shrink_0()
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    action(this, role);
                    cx.notify();
                })
            })
    };
    let open = |url: &String| {
        let url = url.clone();
        a11y::button(format!("{prefix}-sign-in-page"))
            .ghost()
            .label(rust_i18n::t!("models.open_page").to_string())
            .flex_shrink_0()
            .on_click(move |_, _, cx| cx.open_url(&url))
    };
    let cancel = act(
        "sign-in-cancel",
        rust_i18n::t!("action.cancel").into(),
        cancel_sign_in,
    );
    let id = match role {
        ModelRole::Asr => "asr-account",
        ModelRole::Polish => "polish-account",
    };
    let status = |text: SharedString| {
        a11y::status(id, text.clone())
            .flex_1()
            .min_w_0()
            .text_sm()
            .child(text)
    };
    let row = div().h_flex().items_center().gap_2();
    match &form.account {
        Account::Unknown => row
            .child(status("…".into()).text_color(theme.muted_foreground))
            .into_any_element(),
        Account::SignedOut => row
            .child(
                status(rust_i18n::t!("models.signed_out").into())
                    .text_color(theme.muted_foreground),
            )
            .child(act(
                "sign-in",
                rust_i18n::t!("models.sign_in").into(),
                sign_in,
            ))
            .into_any_element(),
        Account::SignedIn => row
            .child(
                div()
                    .flex_shrink_0()
                    .size(px(8.))
                    .rounded_full()
                    .bg(theme.success),
            )
            .child(status(rust_i18n::t!("models.signed_in").into()))
            .child(act(
                "sign-out",
                rust_i18n::t!("models.sign_out").into(),
                sign_out,
            ))
            .into_any_element(),
        Account::Signing { step: None, .. } => row
            .child(
                status(rust_i18n::t!("models.signing_in").into())
                    .text_color(theme.muted_foreground),
            )
            .child(cancel)
            .into_any_element(),
        Account::Signing {
            step: Some(SignInStep::Browser { url }),
            ..
        } => row
            .child(status(rust_i18n::t!("models.in_browser").into()))
            .child(open(url))
            .child(cancel)
            .into_any_element(),
        Account::Signing {
            step: Some(SignInStep::DeviceCode { url, code }),
            ..
        } => {
            let copied = code.clone();
            row.child(
                status(code.clone().into())
                    .font_family("monospace")
                    .text_lg()
                    .font_semibold(),
            )
            .child(
                a11y::button(format!("{prefix}-sign-in-copy"))
                    .ghost()
                    .label(rust_i18n::t!("action.copy").to_string())
                    .flex_shrink_0()
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                    }),
            )
            .child(open(url))
            .child(cancel)
            .into_any_element()
        }
    }
}
