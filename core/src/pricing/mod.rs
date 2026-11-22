//! Built-in price table and call-cost arithmetic (SPEC §2.6).
//!
//! Prices are USD per 1M tokens for the Realtime API, standard tier, copied from
//! <https://developers.openai.com/api/docs/pricing> ("Realtime & Audio") on [`VERIFIED_ON`].
//! **Re-check them whenever the model changes or the prices might have.** Everything is integer
//! nano-USD (1e-9 USD) so the arithmetic is exact: $10.00 per 1M tokens is 10 000 nUSD per token.
//!
//! `response.done`'s `usage` "will correspond to billing": `input_token_details` holds the totals per
//! modality and `cached_tokens_details` the cached **subset** of each, so uncached = total - cached.
//! Input audio transcription is billed separately by OpenAI and is off in our sessions, so it is not
//! included. Unknown models have no price: [`cost_nano_usd`] returns `None` rather than a wrong figure.

use crate::openai::events::Usage;

pub const VERIFIED_ON: &str = "2026-11-21";
pub const SOURCE: &str = "https://developers.openai.com/api/docs/pricing";

/// Per-token rates in nano-USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    pub audio_in: u64,
    pub audio_cached: u64,
    pub audio_out: u64,
    pub text_in: u64,
    pub text_cached: u64,
    pub text_out: u64,
    pub image_in: u64,
    pub image_cached: u64,
}

/// `gpt-realtime-2.1-mini`: audio $10.00 / $0.30 cached / $20.00 out, text $0.60 / $0.06 / $2.40,
/// image $0.80 / $0.08 (per 1M tokens).
const GPT_REALTIME_2_1_MINI: Rates = Rates {
    audio_in: 10_000,
    audio_cached: 300,
    audio_out: 20_000,
    text_in: 600,
    text_cached: 60,
    text_out: 2_400,
    image_in: 800,
    image_cached: 80,
};

/// `gpt-realtime-2.1` (the full model): audio $32.00 / $0.40 cached / $64.00 out, text $4.00 / $0.40 / $24.00,
/// image $5.00 / $0.50 (per 1M tokens).
const GPT_REALTIME_2_1: Rates = Rates {
    audio_in: 32_000,
    audio_cached: 400,
    audio_out: 64_000,
    text_in: 4_000,
    text_cached: 400,
    text_out: 24_000,
    image_in: 5_000,
    image_cached: 500,
};

/// `gemini-3.8-live` (Google Gemini Live), paid tier, per 1M tokens: text $0.75 in / $4.50 out, audio $3.00 in /
/// $12.00 out, image/video $1.00 in. From Google's pricing page via a summary on 2026-11-22: **not yet checked
/// against a real bill**. Cached input is priced like fresh input (no cached rate known; the readout can only
/// err high). Thinking tokens are billed as output and are mapped to output text.
const GEMINI_3_8_LIVE: Rates = Rates {
    audio_in: 3_000,
    audio_cached: 3_000,
    audio_out: 12_000,
    text_in: 750,
    text_cached: 750,
    text_out: 4_500,
    image_in: 1_000,
    image_cached: 1_000,
};

pub fn rates(model: &str) -> Option<&'static Rates> {
    match model {
        "gpt-realtime-2.1-mini" => Some(&GPT_REALTIME_2_1_MINI),
        "gpt-realtime-2.1" => Some(&GPT_REALTIME_2_1),
        "gemini-3.8-live" => Some(&GEMINI_3_8_LIVE),
        _ => None,
    }
}

/// Cost of one response in nano-USD, or `None` if the model has no known price.
pub fn cost_nano_usd(model: &str, u: &Usage) -> Option<u64> {
    let r = rates(model)?;
    let d = &u.input_token_details;
    let c = &d.cached_tokens_details;
    let (mut c_text, mut c_audio, mut c_image) = (c.text_tokens, c.audio_tokens, c.image_tokens);
    if c_text + c_audio + c_image == 0 && d.cached_tokens > 0 {
        // No per-modality breakdown: attribute the cached total to text first, then audio, then image
        // (cached prompt tokens are normally the text instructions).
        let mut left = d.cached_tokens;
        for (cached, total) in [
            (&mut c_text, d.text_tokens),
            (&mut c_audio, d.audio_tokens),
            (&mut c_image, d.image_tokens),
        ] {
            *cached = left.min(total);
            left -= *cached;
        }
    }
    // A cached count can never exceed its modality's total.
    let (c_text, c_audio, c_image) = (
        c_text.min(d.text_tokens),
        c_audio.min(d.audio_tokens),
        c_image.min(d.image_tokens),
    );
    let input = (d.text_tokens - c_text) * r.text_in
        + c_text * r.text_cached
        + (d.audio_tokens - c_audio) * r.audio_in
        + c_audio * r.audio_cached
        + (d.image_tokens - c_image) * r.image_in
        + c_image * r.image_cached;
    let o = &u.output_token_details;
    Some(input + o.text_tokens * r.text_out + o.audio_tokens * r.audio_out)
}

/// nano-USD to whole micro-USD, rounded to nearest.
pub fn to_micro_usd(nano: u64) -> u64 {
    (nano + 500) / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::events::{CachedDetails, TokenDetails};

    const M: &str = "gpt-realtime-2.1-mini";

    fn usage(input: TokenDetails, output: TokenDetails) -> Usage {
        Usage {
            input_token_details: input,
            output_token_details: output,
            ..Usage::default()
        }
    }

    /// The worked example from OpenAI's cost guide (see SPEC §2.6): 2058.84 micro-USD exactly.
    fn documented_example() -> Usage {
        usage(
            TokenDetails {
                text_tokens: 119,
                audio_tokens: 13,
                cached_tokens: 64,
                cached_tokens_details: CachedDetails {
                    text_tokens: 64,
                    ..CachedDetails::default()
                },
                ..TokenDetails::default()
            },
            TokenDetails {
                text_tokens: 30,
                audio_tokens: 91,
                ..TokenDetails::default()
            },
        )
    }

    #[test]
    fn documented_example_costs_exactly_2058_84_micro_usd() {
        // 55 x 0.60 + 64 x 0.06 + 13 x 10 + 30 x 2.40 + 91 x 20 = 2058.84 micro-USD
        let nano = cost_nano_usd(M, &documented_example()).unwrap();
        assert_eq!(nano, 2_058_840);
        assert_eq!(to_micro_usd(nano), 2059);
    }

    #[test]
    fn full_model_prices_the_same_usage_higher() {
        // 119 text (64 cached) in, 13 audio in, 30 text + 91 audio out at the full model's rates:
        // 55 x 4 + 64 x 0.40 + 13 x 32 + 30 x 24 + 91 x 64 = 7 205.6 micro-USD
        let nano = cost_nano_usd("gpt-realtime-2.1", &documented_example()).unwrap();
        assert_eq!(nano, 7_205_600);
    }

    #[test]
    fn cached_tokens_are_a_subset_not_extra() {
        let mut with_cache = documented_example();
        let mut no_cache = documented_example();
        no_cache.input_token_details.cached_tokens = 0;
        no_cache.input_token_details.cached_tokens_details = CachedDetails::default();
        // The same 119 text tokens: caching saves exactly 64 x (0.60 - 0.06) micro-USD, never adds cost.
        let a = cost_nano_usd(M, &no_cache).unwrap();
        let b = cost_nano_usd(M, &with_cache).unwrap();
        assert_eq!(a - b, 64 * (600 - 60));
        with_cache
            .input_token_details
            .cached_tokens_details
            .text_tokens = 9999; // absurd: clamped
        assert_eq!(
            cost_nano_usd(M, &with_cache).unwrap(),
            119 * 60 + 13 * 10_000 + 30 * 2_400 + 91 * 20_000
        );
    }

    #[test]
    fn cached_total_without_breakdown_is_attributed_to_text_first() {
        let mut u = documented_example();
        u.input_token_details.cached_tokens_details = CachedDetails::default();
        assert_eq!(
            cost_nano_usd(M, &u),
            cost_nano_usd(M, &documented_example())
        );
    }

    #[test]
    fn image_tokens_are_priced() {
        let u = usage(
            TokenDetails {
                image_tokens: 1000,
                ..TokenDetails::default()
            },
            TokenDetails::default(),
        );
        assert_eq!(cost_nano_usd(M, &u), Some(1000 * 800));
    }

    #[test]
    fn empty_usage_is_free_and_unknown_model_has_no_price() {
        assert_eq!(cost_nano_usd(M, &Usage::default()), Some(0));
        assert_eq!(cost_nano_usd("gpt-live-1.1", &documented_example()), None);
        assert_eq!(cost_nano_usd("", &documented_example()), None);
    }

    #[test]
    fn audio_output_dominates_a_spoken_reply() {
        // 10 s of bot speech = 200 audio tokens (1 token per 50 ms) = $0.004 at $20/1M.
        let u = usage(
            TokenDetails::default(),
            TokenDetails {
                audio_tokens: 200,
                ..TokenDetails::default()
            },
        );
        assert_eq!(cost_nano_usd(M, &u), Some(4_000_000));
    }
}
