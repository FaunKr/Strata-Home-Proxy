//! How the web UI and the history say how far an expert profile learned.

use crate::api::ExpertCounts;

/// Tokens as the card says them: `950`, `340K`, `1.0M`.
pub fn tokens_short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// A learning time: `2 h 10 min`, `45 min`.
pub fn duration_hm(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}

/// What a round learned: `1.0M tokens, 2 h 10 min`.
pub fn learned_text(tokens: u64, secs: u64) -> String {
    format!("{} tokens, {}", tokens_short(tokens), duration_hm(secs))
}

/// The round against its limits, only the limits set: `340K of 1.0M
/// tokens · 2 h 10 min`.
pub fn progress_text(c: &ExpertCounts) -> String {
    let tokens = match c.token_limit {
        Some(limit) => format!(
            "{} of {} tokens",
            tokens_short(c.tokens),
            tokens_short(limit)
        ),
        None => format!("{} tokens", tokens_short(c.tokens)),
    };
    let time = match c.minute_limit {
        Some(limit) => format!("{} of {}", duration_hm(c.secs), duration_hm(limit * 60)),
        None => duration_hm(c.secs),
    };
    format!("{tokens} · {time}")
}

/// The model card's line for its active profile `name`, learning or not;
/// `None` for the shipped profile.
pub fn card_line(
    name: Option<&str>,
    learning: bool,
    counts: Option<&ExpertCounts>,
) -> Option<String> {
    let name = name?;
    Some(match counts {
        Some(c) if learning => format!("Expert profile: {name}, learning: {}", progress_text(c)),
        _ => format!("Expert profile: {name}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_read_as_k_and_m() {
        assert_eq!(tokens_short(950), "950");
        assert_eq!(tokens_short(340_500), "340K");
        assert_eq!(tokens_short(1_000_000), "1.0M");
    }

    #[test]
    fn a_learning_time_reads_in_hours_and_minutes() {
        assert_eq!(duration_hm(7_800), "2 h 10 min");
        assert_eq!(duration_hm(2_700), "45 min");
    }

    #[test]
    fn progress_names_only_the_limits_set() {
        let mut c = ExpertCounts {
            tokens: 340_000,
            secs: 7_800,
            token_limit: Some(1_000_000),
            ..ExpertCounts::default()
        };
        assert_eq!(progress_text(&c), "340K of 1.0M tokens · 2 h 10 min");
        c.token_limit = None;
        c.minute_limit = Some(180);
        assert_eq!(progress_text(&c), "340K tokens · 2 h 10 min of 3 h 0 min");
    }
}
