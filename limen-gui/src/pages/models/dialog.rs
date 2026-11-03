//! The dialog a provider card opens: a custom server's format, the URL, the
//! chat API where there is a choice, the key, and the model, which can be
//! detected and tested before saving.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::Input;
use gpui_kit::component::select::Select;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, WindowExt as _};
use gpui_kit::*;
use limen_core::{KeyState, ModelRole, Service};

use super::choices::service_label;
use super::form::{EditedField, check, detect, fill, save};
use crate::a11y;
use crate::app::AppView;
use crate::motion;

/// Open the dialog for `service` on `role`'s page, once its settings have
/// loaded.
pub fn open(
    view: &mut AppView,
    role: ModelRole,
    service: Service,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    let form = view.models.form(role);
    if form.loaded.is_none() || form.open {
        return;
    }
    fill(form, role, service, window, cx);
    let entity = cx.entity();
    window.open_dialog(cx, move |dialog, _, cx| {
        body(dialog, entity.clone(), role, service, cx)
    });
}

/// The dialog as `view`'s fields for `role` stand. It is built again on each
/// frame, so it follows every edit and reply.
fn body(
    dialog: Dialog,
    entity: Entity<AppView>,
    role: ModelRole,
    service: Service,
    cx: &App,
) -> Dialog {
    let view = entity.read(cx);
    let (form, prefix, error_id) = match role {
        ModelRole::Asr => (&view.models.asr, "asr", "asr-error"),
        ModelRole::Polish => (&view.models.polish, "polish", "polish-error"),
    };
    let on = |action: fn(&mut AppView, ModelRole, &mut Context<AppView>)| {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| action(this, role, cx))
        }
    };
    let narrow = |select: AnyElement| div().h_flex().child(div().w(px(240.)).child(select));
    let format = service.uses_url().then(|| {
        (
            rust_i18n::t!("models.type").to_string(),
            narrow(
                Select::new(&form.format)
                    .id(SharedString::from(format!("{prefix}-format")))
                    .accessibility_label(rust_i18n::t!("models.type").to_string())
                    .into_any_element(),
            )
            .into_any_element(),
        )
    });
    let url = (
        rust_i18n::t!("models.url").to_string(),
        Input::new(&form.url)
            .accessibility_id(format!("{prefix}-url"))
            .aria_label(rust_i18n::t!("models.url").to_string())
            .into_any_element(),
    );
    // Only polishing chats, and only OpenAI's APIs offer two ways to.
    let chat_api = match role {
        ModelRole::Asr => None,
        ModelRole::Polish => service.chat_api(form.chosen_format),
    }
    .map(|_| {
        (
            rust_i18n::t!("models.chat_api").to_string(),
            narrow(
                Select::new(&form.chat_api)
                    .id(SharedString::from(format!("{prefix}-chat-api")))
                    .accessibility_label(rust_i18n::t!("models.chat_api").to_string())
                    .into_any_element(),
            )
            .into_any_element(),
        )
    });
    // Only the saved service's key can be removed; another service has none.
    let saved_key = form
        .loaded
        .as_ref()
        .is_some_and(|(_, key)| key.state(service) == KeyState::Saved)
        && !form.clear_key;
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
            let entity = entity.clone();
            a11y::button(format!("{prefix}-key-remove"))
                .ghost()
                .label(rust_i18n::t!("models.remove_key").to_string())
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| {
                        let form = this.models.form(role);
                        form.clear_key = true;
                        form.edited(EditedField::Key);
                        form.key.update(cx, |input, cx| {
                            input.set_placeholder(
                                rust_i18n::t!("models.key_removed").to_string(),
                                window,
                                cx,
                            )
                        });
                    })
                })
        }));
    let model = div()
        .h_flex()
        .gap_2()
        .child(
            div().flex_1().min_w_0().child(
                Select::new(&form.model)
                    .id(SharedString::from(format!("{prefix}-model")))
                    .accessibility_label(rust_i18n::t!("models.model").to_string())
                    .placeholder(rust_i18n::t!("models.model").to_string())
                    .search_placeholder(rust_i18n::t!("models.search").to_string()),
            ),
        )
        .child(
            a11y::button(format!("{prefix}-detect"))
                .ghost()
                .label(rust_i18n::t!("models.detect").to_string())
                .flex_shrink_0()
                .disabled(form.detecting())
                .on_click(on(detect)),
        );
    let rows = format.into_iter().chain([url]).chain(chat_api).chain([
        (
            rust_i18n::t!("models.key").to_string(),
            key.into_any_element(),
        ),
        (
            rust_i18n::t!("models.model").to_string(),
            model.into_any_element(),
        ),
    ]);
    let fields = &form.advanced;
    let advanced = [
        ("models.key_header", "key-header", &fields.key_header),
        ("models.headers", "headers", &fields.headers),
        ("models.body", "body", &fields.body),
        ("models.timeout", "timeout", &fields.timeout),
        ("models.retries", "retries", &fields.retries),
    ]
    .map(|(label, id, input)| {
        (
            rust_i18n::t!(label).to_string(),
            Input::new(input)
                .accessibility_id(format!("{prefix}-{id}"))
                .aria_label(rust_i18n::t!(label).to_string())
                .into_any_element(),
        )
    });
    let row = |(label, control): (String, AnyElement)| {
        div()
            .h_flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(88.))
                    .text_sm()
                    .font_medium()
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
    };
    // How far the advanced settings are open, 0 to 1, as the spring
    // follows the toggle; the chevron turns and the rows unfold with it.
    let open = SpringAnimation::new(motion::SPRING).to(match form.show_advanced {
        true => 1.,
        false => 0.,
    });
    let toggle = {
        let entity = entity.clone();
        let id = SharedString::from(format!("{prefix}-advanced"));
        div()
            .id(id.clone())
            .accessibility_id(id.clone())
            .role(Role::Button)
            .aria_label(rust_i18n::t!("models.advanced").to_string())
            .h_flex()
            .items_center()
            .gap_1()
            .px_1()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .active(|style| style.opacity(0.7))
            .child(Icon::new(IconName::ChevronRight).small().with_spring(
                SharedString::from(format!("{id}-chevron")),
                open.clone(),
                |chevron, open| chevron.rotate(radians(open * std::f32::consts::FRAC_PI_2)),
            ))
            .child(rust_i18n::t!("models.advanced").to_string())
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    let form = this.models.form(role);
                    form.show_advanced = !form.show_advanced;
                    cx.notify();
                })
            })
    };
    dialog
        .title(service_label(service))
        .w(px(600.))
        .centered()
        .entrance(DialogEntrance::FadeSlide)
        .child(
            div()
                .v_flex()
                .gap_4()
                .child(
                    // Taller than the window allows once the advanced
                    // settings show, so the fields scroll.
                    div()
                        .id(SharedString::from(format!("{prefix}-fields")))
                        .v_flex()
                        .gap_3()
                        .max_h(px(340.))
                        .overflow_y_scroll()
                        .children(rows.map(row))
                        .child(div().h_flex().child(toggle))
                        .child(
                            // Unfolds to its rows' height on the theme's
                            // spring and stays drawn while folding, so the
                            // fields below slide rather than jump.
                            Collapsible::new()
                                .w_full()
                                .motion_id(SharedString::from(format!("{prefix}-advanced-rows")))
                                .open(form.show_advanced)
                                .content(
                                    div()
                                        .w_full()
                                        .v_flex()
                                        .gap_3()
                                        .pt_3()
                                        .children(advanced.map(row)),
                                ),
                        ),
                )
                .child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().min_w_0().children(form.error.as_ref().map(
                            |error| {
                                a11y::alert(error_id, error.clone())
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child(error.clone())
                            },
                        )))
                        .child(
                            a11y::button(format!("{prefix}-test"))
                                .ghost()
                                .label(rust_i18n::t!("models.test").to_string())
                                .flex_shrink_0()
                                .disabled(!form.can_check())
                                .on_click(on(check)),
                        )
                        .child(
                            a11y::button(format!("{prefix}-save"))
                                .primary()
                                .label(rust_i18n::t!("models.save").to_string())
                                .flex_shrink_0()
                                .disabled(!form.touched || form.saving.is_some())
                                .on_click(on(save)),
                        ),
                ),
        )
}
