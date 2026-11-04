//! The list prices of the transcription models that bill by the minute of
//! audio, which no catalog of chat models knows. Taken from the vendors'
//! pricing as of November 2026; a model missing here is counted as
//! unpriced rather than free.

use crate::config::Service;

/// One model's price per minute of audio, in US dollars, and the least
/// audio a request is billed for, in milliseconds.
struct Rate {
    service: Service,
    model: &'static str,
    per_minute: f64,
    least_ms: u64,
}

const RATES: [Rate; 8] = [
    Rate {
        service: Service::OpenAi,
        model: "gpt-transcribe",
        per_minute: 0.0045,
        least_ms: 0,
    },
    Rate {
        service: Service::OpenAi,
        model: "gpt-4o-transcribe",
        per_minute: 0.006,
        least_ms: 0,
    },
    Rate {
        service: Service::OpenAi,
        model: "gpt-4o-mini-transcribe",
        per_minute: 0.003,
        least_ms: 0,
    },
    Rate {
        service: Service::OpenAi,
        model: "whisper-1",
        per_minute: 0.006,
        least_ms: 0,
    },
    // Groq bills $0.111 and $0.04 an hour, and at least ten seconds a
    // request.
    Rate {
        service: Service::Groq,
        model: "whisper-large-v3",
        per_minute: 0.111 / 60.,
        least_ms: 10_000,
    },
    Rate {
        service: Service::Groq,
        model: "whisper-large-v3-turbo",
        per_minute: 0.04 / 60.,
        least_ms: 10_000,
    },
    Rate {
        service: Service::Mistral,
        model: "voxtral-mini-transcribe-3-0",
        per_minute: 0.0035,
        least_ms: 0,
    },
    Rate {
        service: Service::Mistral,
        model: "voxtral-small-transcribe-3-0",
        per_minute: 0.004,
        least_ms: 0,
    },
];

/// What `audio_ms` of audio costs on `model` of `service`, in US dollars;
/// `None` for a model not billed by the minute, or not listed here.
pub(super) fn per_minute(service: Service, model: &str, audio_ms: u64) -> Option<f64> {
    RATES
        .iter()
        .find(|rate| rate.service == service && rate.model == model.trim())
        .map(|rate| audio_ms.max(rate.least_ms) as f64 / 60_000. * rate.per_minute)
}

#[cfg(test)]
mod tests {
    use super::per_minute;
    use crate::config::Service;

    #[test]
    fn a_short_take_is_billed_as_the_vendor_rounds_it() {
        // Compared to a billionth of a dollar, past float rounding.
        let nano = |cost: Option<f64>| cost.map(|cost| (cost * 1e9).round() as i64);
        assert_eq!(
            [
                per_minute(Service::OpenAi, "whisper-1", 30_000),
                per_minute(Service::Groq, "whisper-large-v3-turbo", 3_000),
                per_minute(Service::Gemini, "gemini-3.8-flash", 30_000),
            ]
            .map(nano),
            [Some(0.003), Some(0.04 / 360.), None].map(nano)
        );
    }
}
