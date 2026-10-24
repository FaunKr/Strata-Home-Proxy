//! The PC service's changelog (`pc-agent/CHANGELOG.md`): what each version
//! brings, for the PC service card while an update is available, and how
//! versions compare: an update is a higher `x.y.z`, a build suffix from an
//! older PC service (`0.2.0+abc1234`) does not count. The proxy
//! carries it built in, so the card shows it while the PC sleeps; the
//! program the NAS serves comes from the same commit.
//!
//! The file has one `## x.y.z` heading per version, newest first, with
//! `- ` lines under it; text before the first heading is for people reading
//! the file. Versions follow semver under 0.x: the minor number goes up when
//! the user has to act on the PC, the patch number for features and fixes.

use std::{
    fmt,
    sync::{Arc, LazyLock},
};

use strata_admin_ui::api::PcServiceChange;

/// The changelog the image carries.
pub const BUILT_IN: &str = include_str!("../../pc-agent/CHANGELOG.md");

/// The version a service that reports none counts as: the one before the
/// changelog began.
const BEFORE_CHANGELOG: Version = Version(0, 1, 0);

/// [`BUILT_IN`], parsed; its unit test keeps it valid.
pub static PARSED: LazyLock<Arc<Changelog>> =
    LazyLock::new(|| Arc::new(Changelog::parse(BUILT_IN).expect("pc-agent/CHANGELOG.md is valid")));

/// A version's numbers; build metadata (`+abc1234`) is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    /// `x.y.z`, optionally with `+build`; `None` for anything else, a
    /// pre-release too.
    fn parse(text: &str) -> Option<Self> {
        let core = text.split_once('+').map_or(text, |(core, _)| core);
        let mut parts = core.split('.').map(|p| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u64>().ok())
                .flatten()
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }

    /// Whether going from `before` to this version needs a step on the PC.
    fn needs_step_after(self, before: Self) -> bool {
        (self.0, self.1) > (before.0, before.1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// `text` as `x.y.z`, without a build suffix; `None` for a version the
/// proxy cannot compare.
pub fn plain(text: &str) -> Option<String> {
    Version::parse(text).map(|v| v.to_string())
}

/// Whether `available` is higher than `installed`. A service without a
/// comparable version counts as the one before the changelog began.
pub fn is_newer(available: &str, installed: Option<&str>) -> bool {
    let installed = installed
        .and_then(Version::parse)
        .unwrap_or(BEFORE_CHANGELOG);
    Version::parse(available).is_some_and(|available| available > installed)
}

/// Whether `installed` is `available` or higher: the service needs no update.
pub fn runs_at_least(installed: Option<&str>, available: &str) -> bool {
    installed.is_some_and(|installed| {
        Version::parse(installed).is_some() && !is_newer(available, Some(installed))
    })
}

#[derive(Debug)]
struct Section {
    version: Version,
    heading: String,
    lines: Vec<String>,
}

/// The versions of a changelog, newest first.
#[derive(Debug)]
pub struct Changelog {
    sections: Vec<Section>,
}

impl Changelog {
    /// Refuses a heading that is no `x.y.z`, versions that do not descend,
    /// a version without lines, other text under a heading, and a file
    /// without versions.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut sections: Vec<Section> = Vec::new();
        for line in text.lines().map(str::trim_end) {
            if let Some(heading) = line.strip_prefix("## ") {
                let heading = heading.trim();
                let version = Version::parse(heading)
                    .filter(|_| !heading.contains('+'))
                    .ok_or_else(|| format!("{heading:?} is no version like 0.3.1"))?;
                if let Some(last) = sections.last() {
                    if last.lines.is_empty() {
                        return Err(format!("{} lists no change", last.heading));
                    }
                    if version >= last.version {
                        return Err(format!("{heading} stands below {}", last.heading));
                    }
                }
                sections.push(Section {
                    version,
                    heading: heading.to_string(),
                    lines: Vec::new(),
                });
            } else if let Some(section) = sections.last_mut() {
                if let Some(entry) = line.strip_prefix("- ") {
                    section.lines.push(entry.trim().to_string());
                } else if !line.trim().is_empty() {
                    return Err(format!(
                        "{line:?} under {} is no \"- \" line",
                        section.heading
                    ));
                }
            }
        }
        match sections.last() {
            None => Err("the changelog lists no version".into()),
            Some(last) if last.lines.is_empty() => Err(format!("{} lists no change", last.heading)),
            Some(_) => Ok(Self { sections }),
        }
    }

    /// The newest version.
    pub fn newest(&self) -> String {
        self.sections[0].heading.clone()
    }

    /// What an update from `installed` to `available` brings: the versions
    /// above `installed` up to `available`, each marked when its minor
    /// number is above that of the version before it in the changelog.
    /// Nothing when `available` is not higher.
    pub fn changes(
        &self,
        installed: Option<&str>,
        available: Option<&str>,
    ) -> Vec<PcServiceChange> {
        let Some(available) = available.and_then(Version::parse) else {
            return Vec::new();
        };
        let installed = installed
            .and_then(Version::parse)
            .unwrap_or(BEFORE_CHANGELOG);
        self.sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.version > installed && s.version <= available)
            .map(|(i, s)| {
                // Against the version before it in the changelog; the first
                // one, which the changelog starts with, needs none.
                let before = self.sections.get(i + 1).map_or(s.version, |b| b.version);
                PcServiceChange {
                    version: s.heading.clone(),
                    needs_step: s.version.needs_step_after(before),
                    lines: s.lines.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `version` in the `[package]` of pc-agent's Cargo.toml.
    fn crate_version() -> String {
        include_str!("../../pc-agent/Cargo.toml")
            .lines()
            .find_map(|l| l.strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("pc-agent's Cargo.toml has a version")
            .to_string()
    }

    #[test]
    fn the_built_in_changelog_is_valid_and_starts_at_the_crate_version() {
        let changelog = Changelog::parse(BUILT_IN).unwrap_or_else(|e| {
            panic!("pc-agent/CHANGELOG.md: {e}");
        });
        assert_eq!(
            changelog.newest(),
            crate_version(),
            "bump pc-agent's version together with a new section in its CHANGELOG.md"
        );
    }

    #[test]
    fn versions_are_numbers_with_optional_build_metadata() {
        assert_eq!(Version::parse("0.2.0+abc1234"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("0.10.3"), Some(Version(0, 10, 3)));
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        for bad in ["dev", "0.2", "0.2.0.1", "0.2.0-rc1", "0.x.0", "", "+abc"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_higher_version_is_newer_and_build_suffixes_do_not_count() {
        assert!(is_newer("0.3.0", Some("0.2.0")));
        assert!(is_newer("0.2.1", Some("0.2.0+abc1234")));
        assert!(!is_newer("0.2.0", Some("0.2.0+abc1234")));
        assert!(!is_newer("0.2.0", Some("0.3.0")));
        assert!(is_newer("0.2.0", None));
        assert!(is_newer("0.2.0", Some("dev")));
        assert!(!is_newer("dev", None));
        assert!(runs_at_least(Some("0.2.0+abc1234"), "0.2.0"));
        assert!(runs_at_least(Some("0.3.0"), "0.2.0"));
        assert!(!runs_at_least(Some("0.2.0"), "0.2.1"));
        assert!(!runs_at_least(Some("dev"), "0.2.0"));
        assert!(!runs_at_least(None, "0.2.0"));
        assert_eq!(plain("0.2.0+abc1234").as_deref(), Some("0.2.0"));
        assert_eq!(plain("dev"), None);
    }

    #[test]
    fn a_changelog_with_rising_empty_or_odd_sections_is_refused() {
        for (text, why) in [
            ("## 0.2.0\n- a\n## 0.3.0\n- b\n", "below"),
            ("## 0.2.0\n- a\n## 0.2.0\n- b\n", "below"),
            ("## 0.3.0\n## 0.2.0\n- b\n", "no change"),
            ("## 0.3.0\n- a\n## 0.2.0\n", "no change"),
            ("## 0.3\n- a\n", "no version"),
            ("## 0.3.0+abc\n- a\n", "no version"),
            ("## 0.3.0\nsome text\n", "no \"- \" line"),
            ("# Title only\n", "no version"),
        ] {
            let err = Changelog::parse(text).unwrap_err();
            assert!(err.contains(why), "{text:?}: {err}");
        }
    }
}
