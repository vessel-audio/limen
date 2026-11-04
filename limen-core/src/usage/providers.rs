//! The ledger grouped by provider: what each served over a span of days,
//! model by model and day by day, as a provider's dashboard shows it.

use super::{Ledger, Tally, UsageRow};
use crate::config::Service;

/// What one provider served over a span of days.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    pub service: Service,
    /// Each of its models, as [`Ledger::since`] orders them.
    pub models: Vec<UsageRow>,
    /// Each day's sum, from the span's first day to its last, days with
    /// no requests included.
    pub daily: Vec<Tally>,
    pub total: Tally,
}

impl Ledger {
    /// Each provider's usage from `first_day` through `last_day`, the
    /// costliest first, then the busiest.
    pub fn providers(&self, first_day: i64, last_day: i64) -> Vec<ProviderUsage> {
        let span = usize::try_from(last_day - first_day + 1).unwrap_or(0);
        let mut providers = self.since(first_day).into_iter().fold(
            Vec::<ProviderUsage>::new(),
            |mut providers, row| {
                match providers
                    .iter_mut()
                    .find(|provider| provider.service == row.service)
                {
                    Some(provider) => provider.models.push(row),
                    None => providers.push(ProviderUsage {
                        service: row.service,
                        models: vec![row],
                        daily: vec![Tally::default(); span],
                        total: Tally::default(),
                    }),
                }
                providers
            },
        );
        for entry in self
            .entries
            .iter()
            .filter(|entry| (first_day..=last_day).contains(&entry.day))
        {
            if let Some(provider) = providers
                .iter_mut()
                .find(|provider| provider.service == entry.service)
            {
                let day = (entry.day - first_day) as usize;
                provider.daily[day] = provider.daily[day] + entry.tally;
                provider.total = provider.total + entry.tally;
            }
        }
        providers.sort_by(|left, right| {
            right
                .total
                .cost_usd
                .total_cmp(&left.total.cost_usd)
                .then(right.total.requests.cmp(&left.total.requests))
        });
        providers
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderUsage;
    use crate::config::Service;
    use crate::models::ModelRole;
    use crate::usage::{Ledger, Tally, UsageRow};

    fn spent(requests: u64, cost_usd: f64) -> Tally {
        Tally {
            requests,
            cost_usd,
            ..Tally::default()
        }
    }

    #[test]
    fn each_provider_sums_its_models_day_by_day_within_the_span() {
        let mut ledger = Ledger::default();
        ledger.record(8, ModelRole::Asr, Service::Groq, "whisper", spent(9, 9.));
        ledger.record(10, ModelRole::Asr, Service::Groq, "whisper", spent(2, 0.25));
        ledger.record(12, ModelRole::Polish, Service::Groq, "llama", spent(1, 0.5));
        ledger.record(
            11,
            ModelRole::Polish,
            Service::DeepSeek,
            "flash",
            spent(4, 1.),
        );

        assert_eq!(
            ledger.providers(10, 12),
            vec![
                ProviderUsage {
                    service: Service::DeepSeek,
                    models: vec![UsageRow {
                        role: ModelRole::Polish,
                        service: Service::DeepSeek,
                        model: "flash".to_string(),
                        tally: spent(4, 1.),
                    }],
                    daily: vec![Tally::default(), spent(4, 1.), Tally::default()],
                    total: spent(4, 1.),
                },
                ProviderUsage {
                    service: Service::Groq,
                    models: vec![
                        UsageRow {
                            role: ModelRole::Asr,
                            service: Service::Groq,
                            model: "whisper".to_string(),
                            tally: spent(2, 0.25),
                        },
                        UsageRow {
                            role: ModelRole::Polish,
                            service: Service::Groq,
                            model: "llama".to_string(),
                            tally: spent(1, 0.5),
                        },
                    ],
                    daily: vec![spent(2, 0.25), Tally::default(), spent(1, 0.5)],
                    total: spent(3, 0.75),
                },
            ]
        );
    }
}
