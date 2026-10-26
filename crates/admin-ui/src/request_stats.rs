//! What the tab "Requests" shows above the list: the averages per model and
//! the points of its two speed charts, for the rows the key filter lets
//! through. Pure, so the numbers are tested without a browser.

use crate::api::{RequestRow, RequestState};

/// The colors the charts have: `--series-1` to `--series-8` in the style
/// sheet, then `--series-other`, which the models past eight share.
pub const COLORS: usize = 9;

/// The averages of one model, over its finished rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAverages {
    pub model: String,
    /// Its place among the models, which picks its color.
    pub color: usize,
    pub requests: usize,
    /// Σ tokens ÷ Σ time, over the rows that have both.
    pub prefill_tok_s: Option<f64>,
    pub generation_tok_s: Option<f64>,
    /// The mean over the rows that have one.
    pub first_token_ms: Option<f64>,
}

/// One request in a chart.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartPoint {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub color: usize,
    pub model: String,
    /// `x` as the axis and the tooltip say it.
    pub x_text: String,
}

impl ChartPoint {
    /// What the dot's tooltip says.
    pub fn title(&self) -> String {
        format!("{}: {:.1} tok/s, {}", self.model, self.y, self.x_text)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RequestStats {
    /// By model name.
    pub models: Vec<ModelAverages>,
    /// Generation tok/s over the arrival time (Unix seconds).
    pub over_time: Vec<ChartPoint>,
    /// Generation tok/s over the prompt tokens, cached ones included.
    pub by_prompt: Vec<ChartPoint>,
}

/// The rows `key` lets through: all of them for `None`.
pub fn filtered<'a>(
    rows: &'a [RequestRow],
    key: Option<&'a str>,
) -> impl Iterator<Item = &'a RequestRow> + 'a {
    rows.iter().filter(move |r| key.is_none_or(|k| r.key == k))
}

/// The key names in the log, for the filter.
pub fn keys(rows: &[RequestRow]) -> Vec<String> {
    let mut keys: Vec<String> = rows.iter().map(|r| r.key.clone()).collect();
    keys.sort();
    keys.dedup();
    keys
}

pub fn request_stats(rows: &[RequestRow], key: Option<&str>) -> RequestStats {
    #[derive(Default)]
    struct Sums {
        requests: usize,
        prefill_tokens: u64,
        prefill_ms: f64,
        generation_tokens: u64,
        generation_ms: f64,
        first_token_ms_total: u64,
        with_first_token: u64,
    }
    let finished: Vec<(&RequestRow, &str)> = filtered(rows, key)
        .filter(|r| r.state == RequestState::Finished)
        .filter_map(|r| Some((r, r.model.as_deref()?)))
        .collect();
    let mut sums = std::collections::BTreeMap::<&str, Sums>::new();
    for &(row, model) in &finished {
        let s = sums.entry(model).or_default();
        s.requests += 1;
        // The rows the list shows a speed for, so a row without one
        // doesn't add tokens without time.
        if let (Some(_), Some(tokens), Some(ms)) =
            (row.prefill_tok_s(), row.prefill_tokens, row.prefill_ms)
        {
            s.prefill_tokens += tokens;
            s.prefill_ms += ms;
        }
        if let (Some(_), Some(tokens), Some(ms)) = (
            row.generation_tok_s(),
            row.generation_tokens,
            row.generation_ms,
        ) {
            s.generation_tokens += tokens;
            s.generation_ms += ms;
        }
        if let Some(ms) = row.first_token_ms {
            s.first_token_ms_total += ms;
            s.with_first_token += 1;
        }
    }
    let speed =
        |tokens: u64, ms: f64| (tokens > 0 && ms > 0.0).then(|| tokens as f64 * 1000.0 / ms);
    let models: Vec<ModelAverages> = sums
        .into_iter()
        .enumerate()
        .map(|(i, (model, s))| ModelAverages {
            model: model.to_string(),
            color: i.min(COLORS - 1),
            requests: s.requests,
            prefill_tok_s: speed(s.prefill_tokens, s.prefill_ms),
            generation_tok_s: speed(s.generation_tokens, s.generation_ms),
            first_token_ms: (s.with_first_token > 0)
                .then(|| s.first_token_ms_total as f64 / s.with_first_token as f64),
        })
        .collect();
    let color = |model: &str| {
        models
            .iter()
            .find(|m| m.model == model)
            .map_or(COLORS - 1, |m| m.color)
    };
    let mut over_time = Vec::new();
    let mut by_prompt = Vec::new();
    // Oldest first, so the newest dots are drawn on top.
    for &(row, model) in finished.iter().rev() {
        let Some(speed) = row.generation_tok_s() else {
            continue;
        };
        let point = |x: f64, x_text: String| ChartPoint {
            id: row.id,
            x,
            y: speed,
            color: color(model),
            model: model.to_string(),
            x_text,
        };
        over_time.push(point(row.at as f64, row.time.clone()));
        if let Some(prompt) = row.prompt_tokens {
            by_prompt.push(point(prompt as f64, format!("{prompt} prompt tokens")));
        }
    }
    RequestStats {
        models,
        over_time,
        by_prompt,
    }
}

/// Round tick values from 0 to at least `max`, about `count` of them past 0.
pub fn ticks(max: f64, count: usize) -> Vec<f64> {
    if max.is_nan() || max <= 0.0 {
        return vec![0.0, 1.0];
    }
    let rough = max / count.max(1) as f64;
    let magnitude = 10f64.powf(rough.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|f| f * magnitude)
        .find(|&s| s >= rough)
        .unwrap_or(10.0 * magnitude);
    let steps = (max / step).ceil() as usize;
    (0..=steps).map(|i| i as f64 * step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RequestOutcome;

    fn row(id: u64, key: &str, model: &str) -> RequestRow {
        RequestRow {
            id,
            at: 1_000 + id,
            time: format!("12:00:0{id}"),
            key: key.into(),
            model: Some(model.into()),
            ..RequestRow::default()
        }
    }

    fn timed(mut r: RequestRow, prefill: (u64, f64), generation: (u64, f64)) -> RequestRow {
        r.prefill_tokens = Some(prefill.0);
        r.prefill_ms = Some(prefill.1);
        r.generation_tokens = Some(generation.0);
        r.generation_ms = Some(generation.1);
        r
    }

    #[test]
    fn averages_are_total_tokens_over_total_time() {
        // 10 tokens in 1 s and 1000 tokens in 9 s: 1010 in 10 s, not the
        // mean of 10 and 111 tok/s.
        let rows = [
            timed(row(2, "App", "m"), (100, 100.0), (1000, 9000.0)),
            timed(row(1, "App", "m"), (300, 900.0), (10, 1000.0)),
        ];
        let stats = request_stats(&rows, None);
        assert_eq!(stats.models.len(), 1);
        let m = &stats.models[0];
        assert_eq!(m.requests, 2);
        assert_eq!(m.generation_tok_s, Some(101.0));
        assert_eq!(m.prefill_tok_s, Some(400.0));
    }

    #[test]
    fn a_row_without_a_number_is_left_out_of_only_that_sum() {
        let mut no_prefill = timed(row(3, "App", "m"), (0, 0.0), (100, 1000.0));
        no_prefill.prefill_tokens = None;
        no_prefill.prefill_ms = None;
        no_prefill.first_token_ms = Some(300);
        let mut no_timings = row(2, "App", "m");
        no_timings.first_token_ms = None;
        let mut first = timed(row(1, "App", "m"), (200, 1000.0), (100, 3000.0));
        first.first_token_ms = Some(100);
        let stats = request_stats(&[no_prefill, no_timings, first], None);
        let m = &stats.models[0];
        assert_eq!(m.requests, 3);
        assert_eq!(m.prefill_tok_s, Some(200.0));
        assert_eq!(m.generation_tok_s, Some(50.0));
        assert_eq!(m.first_token_ms, Some(200.0));
        // Only rows with a generation speed are dots.
        assert_eq!(stats.over_time.len(), 2);
    }

    #[test]
    fn tokens_without_time_add_nothing() {
        // Strata's timings can say 0 ms for a single token; the list shows
        // no speed for such a row, so it isn't in the averages either.
        let instant = timed(row(2, "App", "m"), (5, 0.0), (1, 0.0));
        let timed_row = timed(row(1, "App", "m"), (100, 1000.0), (50, 1000.0));
        let m = &request_stats(&[instant, timed_row], None).models[0];
        assert_eq!(m.requests, 2);
        assert_eq!(m.prefill_tok_s, Some(100.0));
        assert_eq!(m.generation_tok_s, Some(50.0));
    }

    #[test]
    fn the_key_filter_picks_the_rows_of_one_key() {
        let rows = [
            timed(row(3, "Code benchmark", "a"), (1, 1.0), (10, 100.0)),
            timed(row(2, "App", "b"), (1, 1.0), (10, 200.0)),
            row(1, "App", "b"),
        ];
        assert_eq!(keys(&rows), ["App", "Code benchmark"]);
        let app = request_stats(&rows, Some("App"));
        assert_eq!(app.models.len(), 1);
        assert_eq!(app.models[0].model, "b");
        assert_eq!(app.models[0].requests, 2);
        assert_eq!(app.over_time.len(), 1);
        assert_eq!(request_stats(&rows, None).models.len(), 2);
        assert_eq!(filtered(&rows, Some("App")).count(), 2);
    }

    #[test]
    fn models_are_colored_by_their_order_in_the_table() {
        let mut rows: Vec<RequestRow> = ["zeta", "alpha", "mid"]
            .into_iter()
            .enumerate()
            .map(|(i, m)| timed(row(i as u64, "App", m), (1, 1.0), (10, 100.0)))
            .collect();
        rows.extend((0..9).map(|i| row(10 + i, "App", &format!("n{i}"))));
        let stats = request_stats(&rows, None);
        let names: Vec<&str> = stats.models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(&names[..3], ["alpha", "mid", "n0"]);
        let colors: Vec<usize> = stats.models.iter().map(|m| m.color).collect();
        assert_eq!(colors, [0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 8, 8]);
        let color_of = |id| stats.over_time.iter().find(|p| p.id == id).unwrap().color;
        assert_eq!(color_of(1), 0);
        assert_eq!(color_of(2), 1);
        assert_eq!(color_of(0), 8);
    }

    #[test]
    fn rows_on_their_way_or_without_a_model_are_not_counted() {
        let mut running = timed(row(3, "App", "m"), (1, 1.0), (10, 100.0));
        running.state = RequestState::Running;
        let mut refused = row(2, "App", "m");
        refused.model = None;
        refused.outcome = RequestOutcome::Lost;
        let stats = request_stats(&[running, refused], None);
        assert_eq!(stats, RequestStats::default());
    }

    #[test]
    fn empty_input_gives_nothing() {
        assert_eq!(request_stats(&[], None), RequestStats::default());
        assert_eq!(request_stats(&[], Some("App")), RequestStats::default());
        assert!(keys(&[]).is_empty());
    }

    #[test]
    fn the_prompt_chart_counts_cached_tokens_and_skips_rows_without_a_prompt() {
        let mut cached = timed(row(2, "App", "m"), (100, 100.0), (10, 100.0));
        cached.prompt_tokens = Some(5000);
        cached.cached_tokens = Some(4900);
        let no_prompt = timed(row(1, "App", "m"), (1, 1.0), (10, 100.0));
        let stats = request_stats(&[cached, no_prompt], None);
        assert_eq!(stats.by_prompt.len(), 1);
        assert_eq!(stats.by_prompt[0].x, 5000.0);
        assert_eq!(stats.by_prompt[0].y, 100.0);
        assert_eq!(
            stats.by_prompt[0].title(),
            "m: 100.0 tok/s, 5000 prompt tokens"
        );
        // Oldest first.
        assert_eq!(stats.over_time[0].id, 1);
        assert_eq!(stats.over_time[1].title(), "m: 100.0 tok/s, 12:00:02");
    }

    #[test]
    fn ticks_are_round_and_reach_the_top() {
        assert_eq!(ticks(37.0, 4), [0.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(ticks(100.0, 4), [0.0, 25.0, 50.0, 75.0, 100.0]);
        assert_eq!(ticks(7300.0, 3), [0.0, 2500.0, 5000.0, 7500.0]);
        assert_eq!(ticks(0.0, 4), [0.0, 1.0]);
    }
}
