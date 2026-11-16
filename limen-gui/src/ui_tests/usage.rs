//! The Usage page under Models: each provider's dashboard card and the
//! order the cards are dragged into. The Linux desktop suite does not repeat
//! them: the page only reads files the daemon writes, which the bridge's own
//! tests cover.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui_kit::TestAppContext;
use limen_core::{Paths, Service, read_card_order, read_usage};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::bridge::{BridgeCommand, BridgeEvent, ModelRequest};

/// Open the Usage page, which asks for the balances and the ledger, and
/// answer as the bridge does, with no balances and a ledger of three
/// providers: Groq and DeepSeek used today, Groq costing more, and Together
/// ten days back, at no known price. The bridge reads the daemon's files with
/// the functions used here. Returns what the page asked for.
fn open_usage(cx: &mut TestAppContext) -> (Ui<'_>, Vec<BridgeCommand>) {
    static OPENED: AtomicUsize = AtomicUsize::new(0);
    let today = limen_core::today();
    let ledger = format!(
        r#"{{"entries":[
            {{"day":{today},"role":"asr","service":"groq","model":"whisper-large-v3","requests":3,"audio_ms":95000,"cost_usd":0.005}},
            {{"day":{today},"role":"polish","service":"deepseek","model":"deepseek-chat","requests":4,"failures":1,"input_tokens":1200,"output_tokens":300,"cost_usd":0.002}},
            {{"day":{},"role":"polish","service":"together","model":"glm","requests":2,"unpriced":2}}
        ]}}"#,
        today - 10
    );
    let dir = std::env::temp_dir().join(format!(
        "limen-ui-tests-{}-usage-{}",
        std::process::id(),
        OPENED.fetch_add(1, Ordering::Relaxed)
    ));
    let paths = Paths {
        config_file: dir.join("config.toml"),
        socket: dir.join("limen.sock"),
        log_file: dir.join("daemon.log"),
        cache_dir: dir.join("cache"),
    };
    std::fs::create_dir_all(&dir).expect("create the daemon's directory");
    std::fs::write(paths.usage_file(), ledger).expect("write the ledger");
    let read = (
        read_usage(&paths).expect("read the ledger"),
        read_card_order(&paths).expect("read the card order"),
    );
    let _ = std::fs::remove_dir_all(&dir);
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("usage-page");
    let asked = ui.sent();
    ui.receive(BridgeEvent::Balances(Vec::new()));
    ui.receive(BridgeEvent::UsageRead(Ok(read)));
    (ui, asked)
}

/// Each provider has its own card: what it cost, its rate per day over the
/// chosen days, its requests, a column a day, and its models. A provider
/// unused in those days has none until the days reach back to it.
#[gpui_kit::test]
fn each_provider_has_its_own_dashboard(cx: &mut TestAppContext) {
    let (mut ui, asked) = open_usage(cx);
    let week = (
        ui.label("current-page"),
        ui.label("usage-total"),
        ui.label("usage-groq-cost"),
        ui.label("usage-groq-rate"),
        ui.label("usage-groq-model-0"),
        ui.label("usage-deepseek-requests"),
        // Today's column is the last of seven, and names its day first.
        ui.label("usage-groq-day-6").ends_with(" · $0.0050"),
        ui.try_find("usage-card-together").is_some(),
    );
    ui.click("usage-month");
    let month = (
        ui.label("usage-together-cost"),
        ui.label("usage-together-rate"),
        ui.label("usage-total"),
    );
    assert_eq!(
        (asked, week, month),
        (
            vec![
                BridgeCommand::Model(ModelRequest::AskBalances),
                BridgeCommand::Model(ModelRequest::ReadUsage),
            ],
            (
                t!("nav.usage").into(),
                "$0.0070".to_string(),
                "$0.0050".to_string(),
                // $0.0050 over seven days.
                "$0.0007".to_string(),
                format!(
                    "whisper-large-v3 · {} · {} · 1:35 · $0.0050",
                    t!("usage.asr"),
                    t!("usage.requests", count = 3)
                ),
                format!("4 · {}", t!("usage.failed", count = 1)),
                true,
                false,
            ),
            // Ten days back, only the month holds Together, charted by
            // requests, and its unpriced answers make the total a lower bound.
            ("—".to_string(), "0.1".to_string(), "≥ $0.0070".to_string()),
        )
    );
}

/// A card dragged onto another takes its place, and the order is saved;
/// providers new to the view follow the ones placed.
#[gpui_kit::test]
fn cards_are_dragged_into_order(cx: &mut TestAppContext) {
    let (mut ui, _) = open_usage(cx);
    // Cards sit two to a row, so one comes before another by row, then column.
    let place = |ui: &mut Ui, id: &str| {
        let bounds = ui.find(id).bounds();
        (bounds.top(), bounds.left())
    };
    let leads = |ui: &mut Ui, first: &str, second: &str| place(ui, first) < place(ui, second);
    let costlier_leads = leads(&mut ui, "usage-card-groq", "usage-card-deepseek");
    let (from, to) = (
        ui.find("usage-card-deepseek").bounds().center(),
        ui.find("usage-card-groq").bounds().center(),
    );
    ui.drag(from, to);
    let saved = ui.sent();
    let dragged_leads = leads(&mut ui, "usage-card-deepseek", "usage-card-groq");
    ui.click("usage-month");
    let month_in_order = (
        leads(&mut ui, "usage-card-deepseek", "usage-card-groq"),
        leads(&mut ui, "usage-card-groq", "usage-card-together"),
    );
    assert_eq!(
        (costlier_leads, saved, dragged_leads, month_in_order),
        (
            true,
            vec![BridgeCommand::Model(ModelRequest::SaveCardOrder(vec![
                Service::DeepSeek,
                Service::Groq,
            ]))],
            true,
            (true, true),
        )
    );
}
