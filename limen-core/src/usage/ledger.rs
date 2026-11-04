//! The usage ledger: per local day, what each model of each provider
//! served each role.

use serde::{Deserialize, Serialize};

use crate::config::Service;
use crate::models::ModelRole;

/// Every day's counts, oldest first within each model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub entries: Vec<Entry>,
}

/// What one model of one provider served one role on one day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The local day, as [`super::local_day`] counts it.
    pub day: i64,
    pub role: ModelRole,
    pub service: Service,
    pub model: String,
    #[serde(flatten)]
    pub tally: Tally,
}

/// Counts of requests and what they used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    /// Requests sent, each try of a retried one counted.
    pub requests: u64,
    /// Requests that failed, timed out, or were abandoned.
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Audio transcribed, in milliseconds.
    pub audio_ms: u64,
    /// The known cost in US dollars.
    pub cost_usd: f64,
    /// Requests that were answered at an unknown cost, so `cost_usd` is
    /// only a lower bound when any are.
    pub unpriced: u64,
}

impl Tally {
    /// One request that got no answer.
    pub(crate) fn failure() -> Self {
        Self {
            requests: 1,
            failures: 1,
            ..Self::default()
        }
    }
}

impl std::ops::Add for Tally {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            requests: self.requests + other.requests,
            failures: self.failures + other.failures,
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            audio_ms: self.audio_ms + other.audio_ms,
            cost_usd: self.cost_usd + other.cost_usd,
            unpriced: self.unpriced + other.unpriced,
        }
    }
}

/// What one model of one provider served one role over some days.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub role: ModelRole,
    pub service: Service,
    pub model: String,
    pub tally: Tally,
}

impl Ledger {
    pub(crate) fn record(
        &mut self,
        day: i64,
        role: ModelRole,
        service: Service,
        model: &str,
        tally: Tally,
    ) {
        match self.entries.iter_mut().find(|entry| {
            entry.day == day
                && entry.role == role
                && entry.service == service
                && entry.model == model
        }) {
            Some(entry) => entry.tally = entry.tally + tally,
            None => self.entries.push(Entry {
                day,
                role,
                service,
                model: model.to_string(),
                tally,
            }),
        }
    }

    pub(crate) fn forget_before(&mut self, day: i64) {
        self.entries.retain(|entry| entry.day >= day);
    }

    /// Each model's usage from `first_day` on, the ASR models first, then
    /// by requests, most first.
    pub fn since(&self, first_day: i64) -> Vec<UsageRow> {
        let mut rows = self
            .entries
            .iter()
            .filter(|entry| entry.day >= first_day)
            .fold(Vec::<UsageRow>::new(), |mut rows, entry| {
                match rows.iter_mut().find(|row| {
                    row.role == entry.role
                        && row.service == entry.service
                        && row.model == entry.model
                }) {
                    Some(row) => row.tally = row.tally + entry.tally,
                    None => rows.push(UsageRow {
                        role: entry.role,
                        service: entry.service,
                        model: entry.model.clone(),
                        tally: entry.tally,
                    }),
                }
                rows
            });
        rows.sort_by_key(|row| {
            (
                match row.role {
                    ModelRole::Asr => 0,
                    ModelRole::Polish => 1,
                },
                std::cmp::Reverse(row.tally.requests),
            )
        });
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::{Ledger, Tally, UsageRow};
    use crate::config::Service;
    use crate::models::ModelRole;

    fn requests(requests: u64) -> Tally {
        Tally {
            requests,
            ..Tally::default()
        }
    }

    #[test]
    fn days_before_the_first_are_left_out_and_the_rest_summed_per_model() {
        let mut ledger = Ledger::default();
        ledger.record(
            9,
            ModelRole::Polish,
            Service::DeepSeek,
            "flash",
            requests(5),
        );
        ledger.record(
            10,
            ModelRole::Polish,
            Service::DeepSeek,
            "flash",
            requests(1),
        );
        ledger.record(
            11,
            ModelRole::Polish,
            Service::DeepSeek,
            "flash",
            requests(2),
        );
        ledger.record(11, ModelRole::Polish, Service::Groq, "llama", requests(4));
        ledger.record(11, ModelRole::Asr, Service::Custom, "whisper", requests(1));

        assert_eq!(
            ledger.since(10),
            vec![
                UsageRow {
                    role: ModelRole::Asr,
                    service: Service::Custom,
                    model: "whisper".to_string(),
                    tally: requests(1),
                },
                UsageRow {
                    role: ModelRole::Polish,
                    service: Service::Groq,
                    model: "llama".to_string(),
                    tally: requests(4),
                },
                UsageRow {
                    role: ModelRole::Polish,
                    service: Service::DeepSeek,
                    model: "flash".to_string(),
                    tally: requests(3),
                },
            ]
        );
    }

    #[test]
    fn a_day_forgotten_is_gone_from_the_file_too() {
        let mut ledger = Ledger::default();
        ledger.record(1, ModelRole::Asr, Service::Groq, "w", requests(1));
        ledger.record(2, ModelRole::Asr, Service::Groq, "w", requests(1));
        ledger.forget_before(2);

        let read: Ledger = serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
        assert_eq!(read.since(0), ledger.since(2));
        assert_eq!(read.entries.len(), 1);
    }
}
