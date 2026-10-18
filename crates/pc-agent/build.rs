//! The version the service reports: the crate's version plus the build's
//! commit from `STRATA_BUILD_ID` (set by the CI and the Docker build), else
//! `dev`. Every commit's build thus has its own version.
fn main() {
    println!("cargo:rerun-if-env-changed=STRATA_BUILD_ID");
    let id = std::env::var("STRATA_BUILD_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .unwrap_or_else(|| "dev".to_string());
    // A full commit hash is shortened as git shows it.
    let id = if id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        id[..7].to_string()
    } else {
        id
    };
    let version = std::env::var("CARGO_PKG_VERSION").expect("set by cargo");
    println!("cargo:rustc-env=STRATA_PC_AGENT_VERSION={version}+{id}");
}
