//! rig's model catalog as Limen's price list: the list prices it ships for
//! each vendor's models, which price both rig's replies and those of any
//! other backend whose services report only tokens.

use limen_core::{Api, PriceList, Usage};
use rig_core::catalog::Catalog as RigCatalog;
use rig_core::completion::Usage as RigUsage;

/// The [`PriceList`] of rig's built-in catalog.
pub struct Catalog;

impl PriceList for Catalog {
    fn price(&self, api: Api, model: &str, usage: &Usage) -> Option<f64> {
        let counted = RigUsage::new()
            .input_tokens(usage.input_tokens)
            .output_tokens(usage.output_tokens)
            .cached_input_tokens(usage.cached_input_tokens);
        list_price(api, model, &counted)
    }
}

/// What `usage` costs on `model` of `api` at the catalog's list price.
fn list_price(api: Api, model: &str, usage: &RigUsage) -> Option<f64> {
    usage.input_tokens.or(usage.output_tokens)?;
    let vendor = vendor(api)?;
    let found = RigCatalog::builtin()
        .resolve(&format!("{vendor}/{}", model.trim()))
        .ok()?;
    found
        .spec
        .pricing
        .as_ref()?
        .cost(usage)
        .map(|cost| cost.total)
}

/// What a reply of `model` on `api` used: the tokens rig counted, and the
/// cost the service reported, or else the catalog's list price for them.
pub(crate) fn usage(api: Api, model: &str, usage: &RigUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        output_tokens: usage.output_tokens,
        cost_usd: usage
            .cost
            .as_ref()
            .map(|cost| cost.total)
            .or_else(|| list_price(api, model, usage)),
    }
}

/// The vendor the catalog files `api`'s models under; `None` for a server
/// of the user's own, or a vendor the catalog does not price.
fn vendor(api: Api) -> Option<&'static str> {
    match api {
        Api::OpenAi => Some("openai"),
        Api::Anthropic => Some("anthropic"),
        Api::Gemini => Some("gcp.gemini"),
        Api::DeepSeek => Some("deepseek"),
        Api::Mistral => Some("mistral"),
        Api::Groq => Some("groq"),
        Api::OpenRouter => Some("openrouter"),
        Api::XAi => Some("xai"),
        Api::ChatGpt => Some("chatgpt"),
        Api::Copilot => Some("copilot"),
        Api::Together => Some("together"),
        Api::Cohere => Some("cohere"),
        Api::Kimi => Some("moonshot"),
        Api::Zai => Some("zai"),
        Api::MiniMax => Some("minimax"),
        Api::Compatible | Api::Fireworks | Api::Qwen | Api::Nebius => None,
    }
}

#[cfg(test)]
mod tests {
    use limen_core::{Api, PriceList, Usage};

    use super::Catalog;

    #[test]
    fn a_listed_model_is_priced_and_an_unknown_one_is_not() {
        let usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(1_000_000),
            ..Usage::default()
        };
        // GLM-4.5 lists $0.60 in and $2.20 out per million tokens.
        let priced = [
            Catalog.price(Api::Zai, "glm-4.5", &usage),
            Catalog.price(Api::Zai, "no-such-model", &usage),
            Catalog.price(Api::Nebius, "glm-4.5", &usage),
            Catalog.price(Api::Zai, "glm-4.5", &Usage::default()),
        ]
        .map(|cost| cost.map(|cost| (cost * 100.).round() as i64));
        assert_eq!(priced, [Some(280), None, None, None]);
    }
}
