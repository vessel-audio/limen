//! The Usage page under Models.

use anyhow::{Context, Result, ensure};

use crate::daemon::Scenario;
use crate::session::Session;

/// A usage ledger of three providers: Groq and DeepSeek used today, Groq
/// costing more, and Together ten days back, at no known price.
fn ledger(session: &Session) -> Result<()> {
    let today = limen_core::today();
    let ledger = format!(
        r#"{{"entries":[
            {{"day":{today},"role":"asr","service":"groq","model":"whisper-large-v3","requests":3,"audio_ms":95000,"cost_usd":0.005}},
            {{"day":{today},"role":"polish","service":"deepseek","model":"deepseek-chat","requests":4,"failures":1,"input_tokens":1200,"output_tokens":300,"cost_usd":0.002}},
            {{"day":{},"role":"polish","service":"together","model":"glm","requests":2,"unpriced":2}}
        ]}}"#,
        today - 10
    );
    let file = session.paths().log_file.with_file_name("usage.json");
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, ledger)?;
    Ok(())
}

/// Each provider has its own card: what it cost, its rate per day over the
/// chosen days, its requests, a column a day, and its models.
pub fn each_provider_has_its_own_dashboard() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    ledger(&session)?;
    session.click("usage-page")?;
    session.named("current-page", "Usage")?;
    session.named("usage-total", "$0.0070")?;
    session.named("usage-groq-cost", "$0.0050")?;
    // Over seven days.
    session.named("usage-groq-rate", "$0.0007")?;
    session.named(
        "usage-groq-model-0",
        "whisper-large-v3 · ASR · 3 requests · 1:35 · $0.0050",
    )?;
    session.named("usage-deepseek-requests", "4 · 1 failed")?;
    let today = session.node("usage-groq-day-6")?;
    ensure!(
        today.name.ends_with(" · $0.0050"),
        "today's column is named {:?}",
        today.name
    );
    session.absent("usage-card-together")?;
    // Ten days back, only the month holds Together, charted by requests.
    session.click("usage-month")?;
    session.named("usage-together-cost", "—")?;
    session.named("usage-together-rate", "0.1")?;
    session.named("usage-total", "≥ $0.0070")?;
    Ok(())
}

/// A card dragged onto another takes its place, and the order is kept.
pub fn cards_are_dragged_into_order() -> Result<()> {
    let session = Session::fake(Scenario::default())?;
    ledger(&session)?;
    session.click("usage-page")?;
    let place = |id: &str| -> Result<(i32, i32, i32, i32)> {
        let bounds = session
            .settled(id)?
            .bounds
            .with_context(|| format!("{id} is off screen"))?;
        Ok((bounds.y, bounds.x, bounds.width, bounds.height))
    };
    let (groq, deepseek) = (place("usage-card-groq")?, place("usage-card-deepseek")?);
    ensure!(groq < deepseek, "the costlier Groq does not lead");
    let center = |(y, x, width, height): (i32, i32, i32, i32)| (x + width / 2, y + height / 2);
    session.x11().drag(center(deepseek), center(groq))?;
    session.until("DeepSeek to lead", || {
        Ok(place("usage-card-deepseek")? < place("usage-card-groq")?)
    })?;
    let file = session.paths().log_file.with_file_name("usage-cards.json");
    session.until("the order to be kept", || {
        Ok(std::fs::read_to_string(&file).unwrap_or_default() == r#"["deepseek","groq"]"#)
    })?;
    // Providers new to the view follow the ones placed.
    session.click("usage-month")?;
    let together = place("usage-card-together")?;
    ensure!(
        place("usage-card-deepseek")? < place("usage-card-groq")?
            && place("usage-card-groq")? < together,
        "the month's cards are out of order"
    );
    Ok(())
}
