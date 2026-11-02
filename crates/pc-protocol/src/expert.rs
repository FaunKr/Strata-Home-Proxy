//! Learned expert profiles: Strata's ranking of which experts stay in
//! graphics memory, learned from the requests a model answers. The PC service
//! keeps them in `strata_home\expert-profiles\<model>\<name>.bin` and writes
//! the run config's keys for them; the proxy keeps the limits and counts.

use serde::{Deserialize, Serialize};

/// The name the web UI shows for the profile Strata ships with a model.
pub const STANDARD_PROFILE: &str = "Standard";
/// The longest name a learned profile takes.
pub const PROFILE_NAME_MAX: usize = 40;

/// Why `name` is no name for a learned profile, or `None` when it is one:
/// letters, digits, spaces and dashes, 1 to 40 characters, not
/// [`STANDARD_PROFILE`] and not starting or ending with a space.
pub fn profile_name_problem(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("The profile needs a name.".into());
    }
    if name.chars().count() > PROFILE_NAME_MAX {
        return Some(format!(
            "A profile name has at most {PROFILE_NAME_MAX} characters."
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == ' ' || c == '-')
        || name.trim() != name
    {
        return Some("A profile name holds only letters, digits, spaces and dashes.".into());
    }
    if name.eq_ignore_ascii_case(STANDARD_PROFILE) {
        return Some(format!(
            "\"{STANDARD_PROFILE}\" is the profile Strata ships; choose another name."
        ));
    }
    None
}

/// A model's expert profiles, as `GET /strata` reports them per model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertProfiles {
    /// The profile the model starts with; `None` is the one Strata ships.
    #[serde(default)]
    pub active: Option<String>,
    /// The active profile learns: the engine saves into it.
    #[serde(default)]
    pub learning: bool,
    /// Learning is held for a benchmark: Strata runs without saving.
    #[serde(default)]
    pub held: bool,
    /// The run config has an expert profile or its saving set by hand: the
    /// PC service leaves it alone and takes no profile jobs for the model.
    #[serde(default)]
    pub by_hand: bool,
    /// The learned profiles, by name.
    #[serde(default)]
    pub profiles: Vec<ExpertProfile>,
}

/// A learned expert profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertProfile {
    pub name: String,
    /// The file's size; `None` before the engine first saved it.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Unix seconds of the file's last write; `None` before the first save.
    #[serde(default)]
    pub saved: Option<u64>,
}

/// The profile that learns, as `GET /status` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertLearning {
    /// The PC service's model id.
    pub model: String,
    pub profile: String,
    /// Held for a benchmark: Strata runs without saving and nothing counts.
    #[serde(default)]
    pub held: bool,
    /// The seconds Strata ran with this model learning since this round of
    /// learning started.
    #[serde(default)]
    pub secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_letters_digits_spaces_and_dashes_are_names() {
        assert_eq!(profile_name_problem("coding 2-b"), None);
        assert_eq!(profile_name_problem("Café Ñandú"), None);
    }

    #[test]
    fn names_breaking_the_rules_are_refused() {
        for bad in [
            "", "  ", "a/b", "a.b", " lead", "trail ", "standard", "Standard",
        ] {
            assert!(profile_name_problem(bad).is_some(), "{bad:?}");
        }
        assert!(profile_name_problem(&"x".repeat(41)).is_some());
        assert_eq!(profile_name_problem(&"x".repeat(40)), None);
    }
}
