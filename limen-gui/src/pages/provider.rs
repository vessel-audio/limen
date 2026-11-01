//! The ASR server the daemon transcribes with: one row of the dictation
//! control strip, a server address and a route to choose, with the error
//! below it when a change fails.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt};
use gpui_kit::*;
use limen_core::{ProviderEndpoint, split_provider_url};

use crate::a11y;
use crate::app::AppView;
use crate::bridge::BridgeCommand;

type EndpointSelect = SelectState<SearchableVec<&'static str>>;

/// Width of the labels before the strip's fields, so the fields line up.
pub const LABEL_WIDTH: Pixels = px(80.);

/// State of the ASR server row. The fields follow the daemon until the user
/// edits them, and again once a change the user made has been applied.
pub struct ProviderForm {
    pub server: Entity<InputState>,
    pub endpoint: Entity<EndpointSelect>,
    /// The user edited a field since it last followed the daemon.
    pub touched: bool,
    /// A change is being applied; the fields stay as they are until it ends.
    pub pending: bool,
    /// Why the last change failed.
    pub error: Option<String>,
}

impl ProviderForm {
    /// The row's fields, and the subscriptions that track the user's edits.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, [Subscription; 2]) {
        let server = cx.new(|cx| InputState::new(window, cx).placeholder("http://127.0.0.1:8000"));
        let endpoint = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(ProviderEndpoint::ALL.map(ProviderEndpoint::path).to_vec()),
                None,
                window,
                cx,
            )
        });
        let subscriptions = [
            cx.subscribe_in(&server, window, |this, _, event, _, cx| match event {
                InputEvent::Change => this.provider.edited(),
                InputEvent::PressEnter { .. } => apply_provider_server(this, cx),
                _ => {}
            }),
            cx.subscribe_in(
                &endpoint,
                window,
                |this, _, event: &SelectEvent<SearchableVec<&'static str>>, _, _| match event {
                    SelectEvent::Confirm(_) => this.provider.edited(),
                },
            ),
        ];
        (
            Self {
                server,
                endpoint,
                touched: false,
                pending: false,
                error: None,
            },
            subscriptions,
        )
    }

    fn edited(&mut self) {
        self.touched = true;
        self.error = None;
    }

    /// Record how the change being applied ended. A failed change keeps the
    /// fields as the user left them, ready to correct.
    pub fn finish(&mut self, error: Option<String>) {
        self.pending = false;
        self.touched |= error.is_some();
        self.error = error;
    }
}

pub fn provider(view: &AppView, cx: &mut Context<AppView>, window: &mut Window) -> Div {
    let form = &view.provider;
    // A capped URL ends in an ellipsis; its server may be cut off too.
    let current = view
        .model
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.state.config.provider_url.clone())
        .filter(|url| !url.ends_with('…'));
    // A URL without a host shows whole, so the user sees what to repair.
    let (server, route) = current
        .map(|url| split_provider_url(&url).unwrap_or((url, String::new())))
        .unzip();
    let known = route.as_deref().and_then(ProviderEndpoint::from_path);
    // Follow the daemon, but only where a field differs: setting a value
    // resets the field's cursor and selection, and this runs on every frame.
    if let Some(server) = server
        && !form.touched
        && !form.pending
    {
        if form.server.read(cx).value() != server {
            form.server
                .update(cx, |input, cx| input.set_value(server, window, cx));
        }
        let path = known.map(ProviderEndpoint::path);
        if form.endpoint.read(cx).selected_value().copied() != path {
            form.endpoint.update(cx, |select, cx| match path {
                Some(path) => select.set_selected_value(&path, window, cx),
                None => select.set_selected_index(None, window, cx),
            });
        }
    }
    let chosen = form.endpoint.read(cx).selected_value().is_some();
    let title = rust_i18n::t!("provider.title").to_string();
    let route_label = rust_i18n::t!("provider.route").to_string();
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
                        .w(LABEL_WIDTH)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(title.clone()),
                )
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&form.server)
                            .small()
                            .accessibility_id("provider-server")
                            .aria_label(title),
                    ),
                )
                .child(
                    div().flex_shrink_0().w(px(200.)).child(
                        Select::new(&form.endpoint)
                            .id("provider-endpoint")
                            .small()
                            .accessibility_label(route_label.clone())
                            // Names a hand-edited route the list does not offer.
                            .placeholder(route.filter(|_| known.is_none()).unwrap_or(route_label)),
                    ),
                )
                .child(
                    a11y::button("provider-apply")
                        .small()
                        .outline()
                        .label(rust_i18n::t!("action.apply").to_string())
                        .flex_shrink_0()
                        .disabled(!form.touched || form.pending || !chosen)
                        .on_click(cx.listener(|this, _, _, cx| apply_provider_server(this, cx))),
                ),
        )
        .children(form.error.as_ref().map(|error| {
            a11y::alert("provider-error", error.clone())
                .text_xs()
                .text_color(cx.theme().danger)
                .child(error.clone())
        }))
}

/// Hand the edited server and route to the bridge; the core validates the
/// server. Edits made while it is applied mark the row touched again.
pub fn apply_provider_server(view: &mut AppView, cx: &mut Context<AppView>) {
    let form = &mut view.provider;
    let endpoint = form
        .endpoint
        .read(cx)
        .selected_value()
        .and_then(|path| ProviderEndpoint::from_path(path));
    if let (true, false, Some(endpoint)) = (form.touched, form.pending, endpoint) {
        let server = form.server.read(cx).value().trim().to_string();
        match view
            .commands
            .try_send(BridgeCommand::SetProviderServer { server, endpoint })
        {
            Ok(()) => {
                form.touched = false;
                form.pending = true;
            }
            Err(error) => form.error = Some(error.to_string()),
        }
    }
}
