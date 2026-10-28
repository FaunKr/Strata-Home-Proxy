//! How request energy and energy per 1000 tokens read. Pure, so the texts are
//! tested without a browser.

use crate::api::{EnergyAverage, ModelEnergy, RequestRow};

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

/// A row's request energy, `0.420 Wh · €0.0001`; `–` without one.
pub fn request_energy_text(row: &RequestRow) -> String {
    match (row.energy_wh, row.energy_eur) {
        (Some(wh), Some(eur)) => format!("{} · {}", wh_text(wh), eur_text(eur)),
        (Some(wh), None) => wh_text(wh),
        _ => "–".into(),
    }
}

fn average_text(average: &EnergyAverage) -> String {
    format!(
        "{} · {}",
        wh_text(average.wh_per_1000),
        eur_text(average.eur_per_1000)
    )
}

/// The *Models* tab's line of an installed model; `None` for a model without
/// figures or one that is not installed.
pub fn model_energy_line(energy: &ModelEnergy) -> Option<String> {
    if !energy.installed {
        return None;
    }
    let all = energy.all_time.as_ref()?;
    let recent = energy
        .last_30_days
        .as_ref()
        .map(|a| format!("{} (30 days)", average_text(a)))
        .unwrap_or_else(|| "none in 30 days".into());
    Some(format!(
        "Energy per 1000 tokens: {recent}, {} (all time)",
        average_text(all)
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
    fn an_installed_model_shows_both_averages() {
        assert_eq!(
            model_energy_line(&energy(true, average(1.8))).as_deref(),
            Some(
                "Energy per 1000 tokens: 1.80 Wh · €0.0005 (30 days), 2.10 Wh · €0.0006 (all time)"
            )
        );
    }

    #[test]
    fn a_model_without_recent_requests_says_so() {
        assert_eq!(
            model_energy_line(&energy(true, None)).as_deref(),
            Some("Energy per 1000 tokens: none in 30 days, 2.10 Wh · €0.0006 (all time)")
        );
    }

    #[test]
    fn a_removed_model_shows_no_line() {
        assert_eq!(model_energy_line(&energy(false, average(1.8))), None);
    }

    #[test]
    fn larger_figures_take_fewer_places() {
        assert_eq!(wh_text(12.34), "12.3 Wh");
        assert_eq!(eur_text(1.2), "€1.20");
    }
}
