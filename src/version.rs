use std::cmp::Ordering;

/// Application version resolved at compile time by `build.rs`, falling back to `CARGO_PKG_VERSION`.
pub const APP_VERSION: &str = match option_env!("PROTRACK_APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Parsed version tag normalized without any leading `v` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    pub version: String,
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Option<String>,
    pub build: Option<String>,
}

impl Ord for ParsedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| compare_prereleases(self.prerelease.as_deref(), other.prerelease.as_deref()))
            .then_with(|| self.version.cmp(&other.version))
    }
}

impl PartialOrd for ParsedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prereleases(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a_pre), Some(b_pre)) => compare_prerelease_identifiers(a_pre, b_pre),
    }
}

fn compare_prerelease_identifiers(a: &str, b: &str) -> Ordering {
    let mut a_parts = a.split('.');
    let mut b_parts = b.split('.');

    loop {
        match (a_parts.next(), b_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a_id), Some(b_id)) => {
                let ord = compare_single_identifier(a_id, b_id);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

fn compare_single_identifier(a: &str, b: &str) -> Ordering {
    let a_num = is_all_ascii_digits(a);
    let b_num = is_all_ascii_digits(b);

    match (a_num, b_num) {
        (true, true) => {
            let a_trimmed = a.trim_start_matches('0');
            let b_trimmed = b.trim_start_matches('0');
            a_trimmed
                .len()
                .cmp(&b_trimmed.len())
                .then_with(|| a_trimmed.cmp(b_trimmed))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

fn is_all_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_valid_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn validate_prerelease(pre: &str) -> bool {
    if pre.is_empty() {
        return false;
    }

    for seg in pre.split('.') {
        if seg.is_empty() || !seg.bytes().all(is_valid_ident_char) {
            return false;
        }
        if is_all_ascii_digits(seg) && seg.len() > 1 && seg.starts_with('0') {
            return false;
        }
    }

    true
}

fn validate_build(build: &str) -> bool {
    if build.is_empty() {
        return false;
    }

    build
        .split('.')
        .all(|seg| !seg.is_empty() && seg.bytes().all(is_valid_ident_char))
}

/// Parses and validates a version tag with an optional lowercase `v` prefix.
///
/// Accepts `MAJOR.MINOR.PATCH` (where each segment is non-empty ASCII digits
/// parseable as `u64`, allowing leading zeros for CalVer tags such as
/// `v2026.01.05`), plus optional SemVer 2.0.0 prerelease (`-...`) and build
/// metadata (`+...`).
pub fn parse_version_tag(raw: &str) -> Option<ParsedVersion> {
    let normalized = raw.strip_prefix('v').unwrap_or(raw);
    if normalized.is_empty() {
        return None;
    }

    let (without_build, build) = match normalized.split_once('+') {
        Some((before, after)) => {
            if !validate_build(after) {
                return None;
            }
            (before, Some(after.to_string()))
        }
        None => (normalized, None),
    };

    let (core, prerelease) = match without_build.split_once('-') {
        Some((before, after)) => {
            if !validate_prerelease(after) {
                return None;
            }
            (before, Some(after.to_string()))
        }
        None => (without_build, None),
    };

    let mut core_parts = core.split('.');
    let major_str = core_parts.next()?;
    let minor_str = core_parts.next()?;
    let patch_str = core_parts.next()?;
    if core_parts.next().is_some() {
        return None;
    }

    if !is_all_ascii_digits(major_str)
        || !is_all_ascii_digits(minor_str)
        || !is_all_ascii_digits(patch_str)
    {
        return None;
    }

    let major = major_str.parse::<u64>().ok()?;
    let minor = minor_str.parse::<u64>().ok()?;
    let patch = patch_str.parse::<u64>().ok()?;

    Some(ParsedVersion {
        version: normalized.to_string(),
        major,
        minor,
        patch,
        prerelease,
        build,
    })
}

/// Resolves the application version using:
/// 1. Explicit `PROTRACK_VERSION` override (if set and non-empty after trim)
/// 2. GitHub Actions ref context (`GITHUB_REF_TYPE` == `"tag"` -> `GITHUB_REF_NAME`, else fallback)
/// 3. Highest SemVer-precedence valid version tag among `git_head_tags`
/// 4. `fallback_version` (from `CARGO_PKG_VERSION`)
pub fn resolve_app_version(
    explicit_override: Option<&str>,
    github_ref_type: Option<&str>,
    github_ref_name: Option<&str>,
    git_head_tags: &[String],
    fallback_version: &str,
) -> String {
    if let Some(override_raw) = explicit_override.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_version_tag(override_raw)
            .map(|parsed| parsed.version)
            .unwrap_or_else(|| fallback_version.to_string());
    }

    if let Some(ref_type) = github_ref_type.map(str::trim).filter(|s| !s.is_empty()) {
        if ref_type == "tag" {
            if let Some(ref_name) = github_ref_name.map(str::trim).filter(|s| !s.is_empty()) {
                return parse_version_tag(ref_name)
                    .map(|parsed| parsed.version)
                    .unwrap_or_else(|| fallback_version.to_string());
            }
        }
        return fallback_version.to_string();
    }

    git_head_tags
        .iter()
        .filter_map(|tag| parse_version_tag(tag.trim_end_matches('\r')))
        .max()
        .map(|parsed| parsed.version)
        .unwrap_or_else(|| fallback_version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_version_tags() {
        let cases = [
            ("v2026.11.23", "2026.11.23", 2026, 11, 23, None, None),
            ("2026.11.23", "2026.11.23", 2026, 11, 23, None, None),
            ("v0.1.0", "0.1.0", 0, 1, 0, None, None),
            ("v2026.01.05", "2026.01.05", 2026, 1, 5, None, None),
            (
                "v2026.11.23-beta.1",
                "2026.11.23-beta.1",
                2026,
                11,
                23,
                Some("beta.1"),
                None,
            ),
            (
                "2026.11.23-rc.0",
                "2026.11.23-rc.0",
                2026,
                11,
                23,
                Some("rc.0"),
                None,
            ),
            (
                "v1.0.0-alpha",
                "1.0.0-alpha",
                1,
                0,
                0,
                Some("alpha"),
                None,
            ),
            (
                "1.0.0-0.3.7",
                "1.0.0-0.3.7",
                1,
                0,
                0,
                Some("0.3.7"),
                None,
            ),
            (
                "1.0.0-x.7.z.92",
                "1.0.0-x.7.z.92",
                1,
                0,
                0,
                Some("x.7.z.92"),
                None,
            ),
            (
                "1.0.0-x-y-z.--+build.01",
                "1.0.0-x-y-z.--+build.01",
                1,
                0,
                0,
                Some("x-y-z.--"),
                Some("build.01"),
            ),
            (
                "v2.1.0-rc.1+sha.abc123",
                "2.1.0-rc.1+sha.abc123",
                2,
                1,
                0,
                Some("rc.1"),
                Some("sha.abc123"),
            ),
            (
                "v2.1.0+20130313144700",
                "2.1.0+20130313144700",
                2,
                1,
                0,
                None,
                Some("20130313144700"),
            ),
        ];

        for (raw, expected_ver, maj, min, pat, pre, bld) in cases {
            let parsed = parse_version_tag(raw)
                .unwrap_or_else(|| panic!("expected valid version tag for `{raw}`"));
            assert_eq!(parsed.version, expected_ver, "tag `{raw}`");
            assert_eq!(parsed.major, maj, "tag `{raw}`");
            assert_eq!(parsed.minor, min, "tag `{raw}`");
            assert_eq!(parsed.patch, pat, "tag `{raw}`");
            assert_eq!(parsed.prerelease.as_deref(), pre, "tag `{raw}`");
            assert_eq!(parsed.build.as_deref(), bld, "tag `{raw}`");
        }
    }

    #[test]
    fn rejects_invalid_version_tags() {
        let invalid = [
            "",
            "v",
            "V1.2.3",
            "vv1.2.3",
            "nightly",
            "vfoo",
            "v1",
            "v1.2",
            "v1.2.3.4",
            "v1.2.3foo",
            "v1.2.3-",
            "v1.2.3-alpha.",
            "v1.2.3-.alpha",
            "v1.2.3-alpha..1",
            "v1.2.3-alpha_1",
            "v1.2.3-01",
            "v1.2.3-alpha.01",
            "v1.2.3+",
            "v1.2.3+build..1",
            "v1.2.3+a+b",
            " v1.2.3",
            "v1.2.3 ",
            "v1..3",
            "v.1.2",
        ];

        for raw in invalid {
            assert_eq!(
                parse_version_tag(raw),
                None,
                "expected `{raw}` to be rejected"
            );
        }
    }

    #[test]
    fn orders_versions_by_semver_precedence() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "2026.11.23-rc.1",
            "2026.11.23",
        ];

        let parsed: Vec<ParsedVersion> = chain
            .iter()
            .map(|s| parse_version_tag(s).expect("valid semver in chain"))
            .collect();

        for pair in parsed.windows(2) {
            assert!(
                pair[0] < pair[1],
                "expected `{}` < `{}`",
                pair[0].version,
                pair[1].version
            );
        }
    }

    #[test]
    fn resolves_version_with_expected_precedence() {
        let head_tags = vec!["nightly".to_string(), "v2026.11.23".to_string()];

        // 1. Explicit override wins over CI & git tags
        assert_eq!(
            resolve_app_version(
                Some("v1.2.3-rc.1"),
                Some("tag"),
                Some("v9.9.9"),
                &head_tags,
                "0.1.0",
            ),
            "1.2.3-rc.1"
        );

        // Invalid explicit override falls back instead of leaking into git/CI
        assert_eq!(
            resolve_app_version(
                Some("not-a-version"),
                Some("tag"),
                Some("v9.9.9"),
                &head_tags,
                "0.1.0",
            ),
            "0.1.0"
        );

        // Empty/whitespace explicit override is ignored
        assert_eq!(
            resolve_app_version(Some("   "), None, None, &head_tags, "0.1.0"),
            "2026.11.23"
        );

        // 2. GitHub tag build
        assert_eq!(
            resolve_app_version(None, Some("tag"), Some("v2026.11.23-beta.1"), &[], "0.1.0"),
            "2026.11.23-beta.1"
        );
        assert_eq!(
            resolve_app_version(None, Some("tag"), Some("2026.11.23"), &[], "0.1.0"),
            "2026.11.23"
        );
        assert_eq!(
            resolve_app_version(None, Some("tag"), Some("nightly"), &head_tags, "0.1.0"),
            "0.1.0"
        );

        // GitHub branch/PR build ignores local git tags on HEAD
        assert_eq!(
            resolve_app_version(None, Some("branch"), Some("main"), &head_tags, "0.1.0"),
            "0.1.0"
        );

        // 3. Local git tags pick highest SemVer tag (stable > prerelease, higher > lower)
        let multi_tags = vec![
            "v2026.11.23-rc.1".to_string(),
            "nightly".to_string(),
            "2026.11.23".to_string(),
            "v2026.11.22".to_string(),
        ];
        assert_eq!(
            resolve_app_version(None, None, None, &multi_tags, "0.1.0"),
            "2026.11.23"
        );

        // CRLF tolerance on git output
        assert_eq!(
            resolve_app_version(None, None, None, &["v2026.11.23\r".to_string()], "0.1.0"),
            "2026.11.23"
        );

        // Fallback when no valid git tags exist
        assert_eq!(
            resolve_app_version(None, None, None, &["nightly".to_string()], "0.1.0"),
            "0.1.0"
        );
        assert_eq!(
            resolve_app_version(None, None, None, &[], "0.1.0"),
            "0.1.0"
        );
    }

    #[test]
    fn compiled_app_version_is_valid_and_matches_expected_env() {
        assert!(
            parse_version_tag(APP_VERSION).is_some(),
            "APP_VERSION `{APP_VERSION}` should parse as a valid version"
        );

        if let Ok(expected) = std::env::var("EXPECT_APP_VERSION") {
            assert_eq!(APP_VERSION, expected);
        }
    }
}
