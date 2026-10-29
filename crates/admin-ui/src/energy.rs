//! How request energy and energy per 1000 tokens read, and which token unit
//! the *Models* tab shows them in. The texts are pure, so they are tested
//! without a browser; only the remembered choice touches the browser itself.

use crate::api::{EnergyAverage, ModelEnergy, RequestRow};

/// The token unit the *Models* tab shows a model's energy line in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TokenUnit {
    /// Per 1000 tokens, as the proxy's API counts; the default.
    #[default]
    Thousand,
    /// Per 1 million tokens, as prices for language models are usually quoted.
    Million,
}

impl TokenUnit {
    /// The name in the line and in what the browser stores.
    pub fn name(self) -> &'static str {
        match self {
            Self::Thousand => "1k",
            Self::Million => "1M",
        }
    }

    /// Times a figure that comes per 1000 tokens.
    fn factor(self) -> f64 {
        match self {
            Self::Thousand => 1.0,
            Self::Million => 1000.0,
        }
    }
}

/// The browser's local storage key holding this browser's choice, written as
/// [`TokenUnit::name`]. Only the browser half reads or writes it.
#[cfg(feature = "hydrate")]
const TOKEN_UNIT_KEY: &str = "energy-token-unit";

/// The unit the stored text says: `1M` is million; anything else, or nothing,
/// is thousand.
pub fn unit_from_stored(stored: Option<&str>) -> TokenUnit {
    match stored {
        Some(text) if text == TokenUnit::Million.name() => TokenUnit::Million,
        _ => TokenUnit::Thousand,
    }
}

/// This browser's remembered unit; 1k when nothing is stored or the storage
/// throws on read (a private window, blocked site data).
#[cfg(feature = "hydrate")]
pub fn stored_unit() -> TokenUnit {
    let stored = browser_storage()
        .and_then(|s| s.get_item(TOKEN_UNIT_KEY).ok())
        .flatten();
    unit_from_stored(stored.as_deref())
}

/// No browser, no choice: the server always draws 1k.
#[cfg(not(feature = "hydrate"))]
pub fn stored_unit() -> TokenUnit {
    TokenUnit::default()
}

/// Remembers the unit in this browser. A storage that throws on write counts
/// as nothing stored: the page goes on with the pressed unit for this visit.
#[cfg(feature = "hydrate")]
pub fn store_unit(unit: TokenUnit) {
    if let Some(storage) = browser_storage() {
        let _ = storage.set_item(TOKEN_UNIT_KEY, unit.name());
    }
}

#[cfg(not(feature = "hydrate"))]
pub fn store_unit(_unit: TokenUnit) {}

/// The browser's local storage, or `None` where there is none or the browser
/// refuses access to it.
#[cfg(feature = "hydrate")]
fn browser_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// `0.42 Wh`, with more places the smaller it gets.
pub fn wh_text(wh: f64) -> String {
    if wh >= 10.0 {
        format!("{wh:.1} Wh")
    } else if wh >= 1.0 {
        format!("{wh:.2} Wh")
    } else {
        format!("{wh:.3} Wh")
    }
}

/// `€0.0012` below a cent, `€1.20` above.
pub fn eur_text(eur: f64) -> String {
    if eur >= 0.01 {
        format!("€{eur:.2}")
    } else {
        format!("€{eur:.4}")
    }
}

/// A row's request energy, `0.420 Wh · €0.0001`; `–` without one. Every
/// figure keeps today's Wh text: the kWh form belongs to the energy line only.
pub fn request_energy_text(row: &RequestRow) -> String {
    match (row.energy_wh, row.energy_eur) {
        (Some(wh), Some(eur)) => format!("{} · {}", wh_text(wh), eur_text(eur)),
        (Some(wh), None) => wh_text(wh),
        _ => "–".into(),
    }
}

/// A figure in the energy line: `2.10 kWh` from 1000 Wh up, so a figure per
/// 1M tokens stays short; [`wh_text`] below that.
fn line_wh_text(wh: f64) -> String {
    if wh >= 1000.0 {
        format!("{:.2} kWh", wh / 1000.0)
    } else {
        wh_text(wh)
    }
}

fn average_text(average: &EnergyAverage, unit: TokenUnit) -> String {
    let factor = unit.factor();
    format!(
        "{} · {}",
        line_wh_text(average.wh_per_1000 * factor),
        eur_text(average.eur_per_1000 * factor)
    )
}

/// The *Models* tab's line of an installed model; `None` for a model without
/// figures or one that is not installed.
pub fn model_energy_line(energy: &ModelEnergy, unit: TokenUnit) -> Option<String> {
    if !energy.installed {
        return None;
    }
    let all = energy.all_time.as_ref()?;
    let recent = energy
        .last_30_days
        .as_ref()
        .map(|a| format!("{} (30 days)", average_text(a, unit)))
        .unwrap_or_else(|| "none in 30 days".into());
    Some(format!(
        "Energy per {} tokens: {recent}, {} (all time)",
        unit.name(),
        average_text(all, unit)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn average(wh: f64) -> Option<EnergyAverage> {
        Some(EnergyAverage {
            wh_per_1000: wh,
            eur_per_1000: wh / 1000.0 * 0.30,
            requests: 3,
        })
    }

    fn energy(installed: bool, recent: Option<EnergyAverage>) -> ModelEnergy {
        ModelEnergy {
            model: "IQ3_XXS".into(),
            installed,
            last_30_days: recent,
            all_time: average(2.1),
        }
    }

    #[test]
    fn a_row_shows_its_energy_in_wh_and_euro() {
        let row = RequestRow {
            energy_wh: Some(0.42),
            energy_eur: Some(0.000126),
            ..RequestRow::default()
        };
        assert_eq!(request_energy_text(&row), "0.420 Wh · €0.0001");
        assert_eq!(request_energy_text(&RequestRow::default()), "–");
    }

    #[test]
    fn a_request_of_1000_wh_or_more_still_reads_in_wh() {
        let row = RequestRow {
            energy_wh: Some(1200.0),
            energy_eur: Some(0.36),
            ..RequestRow::default()
        };
        assert_eq!(request_energy_text(&row), "1200.0 Wh · €0.36");
    }

    #[test]
    fn an_installed_model_shows_both_averages() {
        assert_eq!(
            model_energy_line(&energy(true, average(1.8)), TokenUnit::Thousand).as_deref(),
            Some("Energy per 1k tokens: 1.80 Wh · €0.0005 (30 days), 2.10 Wh · €0.0006 (all time)")
        );
    }

    #[test]
    fn a_line_per_1k_tokens_names_1k() {
        assert_eq!(
            model_energy_line(&energy(true, average(1.8)), TokenUnit::Thousand).as_deref(),
            Some("Energy per 1k tokens: 1.80 Wh · €0.0005 (30 days), 2.10 Wh · €0.0006 (all time)")
        );
    }

    #[test]
    fn a_line_per_1m_tokens_multiplies_by_1000() {
        assert_eq!(
            model_energy_line(&energy(true, average(1.8)), TokenUnit::Million).as_deref(),
            Some("Energy per 1M tokens: 1.80 kWh · €0.54 (30 days), 2.10 kWh · €0.63 (all time)")
        );
    }

    #[test]
    fn a_line_per_1m_tokens_keeps_wh_below_1000() {
        assert_eq!(
            model_energy_line(&energy(true, average(0.5)), TokenUnit::Million).as_deref(),
            Some("Energy per 1M tokens: 500.0 Wh · €0.15 (30 days), 2.10 kWh · €0.63 (all time)")
        );
    }

    #[test]
    fn a_model_without_recent_requests_says_so() {
        assert_eq!(
            model_energy_line(&energy(true, None), TokenUnit::Thousand).as_deref(),
            Some("Energy per 1k tokens: none in 30 days, 2.10 Wh · €0.0006 (all time)")
        );
    }

    #[test]
    fn a_model_without_recent_requests_says_so_per_1m() {
        assert_eq!(
            model_energy_line(&energy(true, None), TokenUnit::Million).as_deref(),
            Some("Energy per 1M tokens: none in 30 days, 2.10 kWh · €0.63 (all time)")
        );
    }

    #[test]
    fn a_removed_model_shows_no_line() {
        assert_eq!(
            model_energy_line(&energy(false, average(1.8)), TokenUnit::Thousand),
            None
        );
    }

    #[test]
    fn a_removed_model_shows_no_line_per_1m() {
        assert_eq!(
            model_energy_line(&energy(false, average(1.8)), TokenUnit::Million),
            None
        );
    }

    #[test]
    fn a_stored_1m_reads_as_1m() {
        assert_eq!(unit_from_stored(Some("1M")), TokenUnit::Million);
    }

    #[test]
    fn nothing_stored_reads_as_1k() {
        assert_eq!(unit_from_stored(None), TokenUnit::Thousand);
    }

    #[test]
    fn an_unknown_stored_value_reads_as_1k() {
        assert_eq!(unit_from_stored(Some("2M")), TokenUnit::Thousand);
    }

    #[test]
    fn larger_figures_take_fewer_places() {
        assert_eq!(wh_text(12.34), "12.3 Wh");
        assert_eq!(eur_text(1.2), "€1.20");
    }
}
