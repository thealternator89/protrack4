use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// A single external link attached to a Feature or Work Item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemLink {
    pub url: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Jira,
    GitHub,
    AzureDevOps,
    GitLab,
    Linear,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLink {
    pub kind: LinkKind,
    pub suggested_label: String,
}

/// Parses a URL string and returns the detected scheme kind and pre-populated label.
///
/// Supports Jira tickets (`"ABC-123"`), Azure DevOps work items & PRs (`"#456"`),
/// GitHub issues/PRs/discussions (`"#456"`), GitLab issues/MRs (`"#456"` / `"!456"`),
/// Linear issues (`"ENG-123"`), Bitbucket PRs (`"#456"`), and a host fallback for
/// generic `http(s)` URLs.
pub fn parse_link_url(raw: &str) -> Option<ParsedLink> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }

    let without_scheme = if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "https://") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "http://") {
        rest
    } else {
        // Reject explicit non-http(s) schemes (e.g. javascript:, file:, ftp://, mailto:).
        if let Some(colon_idx) = trimmed.find(':') {
            let slash_idx = trimmed.find('/').unwrap_or(usize::MAX);
            if colon_idx < slash_idx {
                return None;
            }
        }
        trimmed
    };

    // Strip fragment, then query, then split authority and path.
    let (before_frag, _fragment) = match without_scheme.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (without_scheme, None),
    };
    let (before_query, query) = match before_frag.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (before_frag, None),
    };

    let (authority, path) = match before_query.split_once('/') {
        Some((auth, p)) => (auth, p),
        None => (before_query, ""),
    };

    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(h, _)| h)
        .to_ascii_lowercase();

    if host.is_empty() {
        return None;
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // 1. Jira:
    // - Query param selectedIssue=<KEY>
    // - Path .../browse/<KEY>
    // - Path .../projects/<PROJ>/issues/<KEY>
    // - Host containing "atlassian.net" or "jira" with .../issues/<KEY>
    if let Some(q) = query
        && let Some(val) = find_query_param(q, "selectedissue")
        && let Some(key) = normalize_issue_key(val)
    {
        return Some(ParsedLink {
            kind: LinkKind::Jira,
            suggested_label: key,
        });
    }

    for i in 0..segments.len() {
        if segments[i].eq_ignore_ascii_case("browse")
            && let Some(next) = segments.get(i + 1)
            && let Some(key) = normalize_issue_key(next)
        {
            return Some(ParsedLink {
                kind: LinkKind::Jira,
                suggested_label: key,
            });
        }
        if segments[i].eq_ignore_ascii_case("projects")
            && segments.get(i + 2).is_some_and(|s| s.eq_ignore_ascii_case("issues"))
            && let Some(next) = segments.get(i + 3)
            && let Some(key) = normalize_issue_key(next)
        {
            return Some(ParsedLink {
                kind: LinkKind::Jira,
                suggested_label: key,
            });
        }
    }

    if (host.contains("atlassian.net") || host.contains("jira"))
        && let Some(key) = segments
            .windows(2)
            .find(|w| w[0].eq_ignore_ascii_case("issues"))
            .and_then(|w| normalize_issue_key(w[1]))
    {
        return Some(ParsedLink {
            kind: LinkKind::Jira,
            suggested_label: key,
        });
    }

    // 2. Linear: linear.app/<team>/issue/<KEY>/...
    if (host == "linear.app" || host.ends_with(".linear.app"))
        && let Some(key) = segments
            .windows(2)
            .find(|w| w[0].eq_ignore_ascii_case("issue"))
            .and_then(|w| normalize_issue_key(w[1]))
    {
        return Some(ParsedLink {
            kind: LinkKind::Linear,
            suggested_label: key,
        });
    }

    // 3. Azure DevOps:
    // - .../_workitems/edit/<id> or .../_workitems/<id>
    // - .../_queries/edit/<id>
    // - ?workitem=<id> on dev.azure.com / *.visualstudio.com or _boards/_backlogs/_sprints/_queries/_workitems
    // - .../_git/<repo>/pullrequest/<id>
    for i in 0..segments.len() {
        if segments[i].eq_ignore_ascii_case("_workitems") {
            if segments.get(i + 1).is_some_and(|s| s.eq_ignore_ascii_case("edit"))
                && let Some(num) = segments.get(i + 2).and_then(|s| parse_digits(s))
            {
                return Some(ParsedLink {
                    kind: LinkKind::AzureDevOps,
                    suggested_label: format!("#{num}"),
                });
            }
            if let Some(num) = segments.get(i + 1).and_then(|s| parse_digits(s)) {
                return Some(ParsedLink {
                    kind: LinkKind::AzureDevOps,
                    suggested_label: format!("#{num}"),
                });
            }
        }
        if segments[i].eq_ignore_ascii_case("_queries")
            && segments.get(i + 1).is_some_and(|s| s.eq_ignore_ascii_case("edit"))
            && let Some(num) = segments.get(i + 2).and_then(|s| parse_digits(s))
        {
            return Some(ParsedLink {
                kind: LinkKind::AzureDevOps,
                suggested_label: format!("#{num}"),
            });
        }
    }

    let is_azdo_context = host == "dev.azure.com"
        || host.ends_with(".visualstudio.com")
        || segments.iter().any(|s| {
            s.eq_ignore_ascii_case("_boards")
                || s.eq_ignore_ascii_case("_backlogs")
                || s.eq_ignore_ascii_case("_sprints")
                || s.eq_ignore_ascii_case("_queries")
                || s.eq_ignore_ascii_case("_workitems")
        });
    if is_azdo_context
        && let Some(q) = query
        && let Some(val) = find_query_param(q, "workitem")
        && let Some(num) = parse_digits(val)
    {
        return Some(ParsedLink {
            kind: LinkKind::AzureDevOps,
            suggested_label: format!("#{num}"),
        });
    }

    if let Some(git_idx) = segments.iter().position(|s| s.eq_ignore_ascii_case("_git")) {
        for i in (git_idx + 1)..segments.len() {
            if segments[i].eq_ignore_ascii_case("pullrequest")
                && let Some(num) = segments.get(i + 1).and_then(|s| parse_digits(s))
            {
                return Some(ParsedLink {
                    kind: LinkKind::AzureDevOps,
                    suggested_label: format!("#{num}"),
                });
            }
        }
    }

    // 4. GitLab: .../-/issues/<id> -> #id, .../-/merge_requests/<id> -> !id
    for i in 0..segments.len() {
        if segments[i] == "-" {
            if segments.get(i + 1).is_some_and(|s| s.eq_ignore_ascii_case("issues"))
                && let Some(num) = segments.get(i + 2).and_then(|s| parse_digits(s))
            {
                return Some(ParsedLink {
                    kind: LinkKind::GitLab,
                    suggested_label: format!("#{num}"),
                });
            }
            if segments
                .get(i + 1)
                .is_some_and(|s| s.eq_ignore_ascii_case("merge_requests"))
                && let Some(num) = segments.get(i + 2).and_then(|s| parse_digits(s))
            {
                return Some(ParsedLink {
                    kind: LinkKind::GitLab,
                    suggested_label: format!("!{num}"),
                });
            }
        }
    }

    // 5. GitHub & Bitbucket: /<owner>/<repo>/(issues|pull|discussions|pull-requests)/<id>
    for i in 2..segments.len() {
        if segments[i - 1] != "-"
            && (segments[i].eq_ignore_ascii_case("issues")
                || segments[i].eq_ignore_ascii_case("pull")
                || segments[i].eq_ignore_ascii_case("discussions")
                || segments[i].eq_ignore_ascii_case("pull-requests"))
            && let Some(num) = segments.get(i + 1).and_then(|s| parse_digits(s))
        {
            let kind = if host == "github.com" || host.contains("github") {
                LinkKind::GitHub
            } else {
                LinkKind::Generic
            };
            return Some(ParsedLink {
                kind,
                suggested_label: format!("#{num}"),
            });
        }
    }

    // 6. Generic fallback: host without leading "www."
    let display_host = host.strip_prefix("www.").unwrap_or(&host);
    if !display_host.is_empty() && (display_host.contains('.') || display_host == "localhost") {
        return Some(ParsedLink {
            kind: LinkKind::Generic,
            suggested_label: display_host.to_string(),
        });
    }

    None
}

/// Returns the pre-populated label for a URL, if one can be parsed.
pub fn suggest_link_label(raw: &str) -> Option<String> {
    parse_link_url(raw).map(|p| p.suggested_label)
}

/// Validates and normalizes an item link's URL and label.
///
/// Requires an explicit `http://` or `https://` URL with no whitespace and a non-empty host.
/// If `raw_label` is empty after trimming, auto-fills using [`suggest_link_label`], falling back
/// to `"Link"`.
pub fn validate_item_link(raw_url: &str, raw_label: &str) -> Result<ItemLink, DomainError> {
    let url = raw_url.trim();
    if url.is_empty() {
        return Err(DomainError::Validation("Link URL cannot be empty".to_string()));
    }
    if url.chars().any(char::is_whitespace) {
        return Err(DomainError::Validation(
            "Link URL cannot contain spaces".to_string(),
        ));
    }

    let after_scheme = if let Some(rest) = strip_prefix_ignore_ascii_case(url, "https://") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_ascii_case(url, "http://") {
        rest
    } else {
        return Err(DomainError::Validation(
            "Link URL must start with http:// or https://".to_string(),
        ));
    };

    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, rest)| rest);
    let host = host_port.split(':').next().unwrap_or("");
    if host.is_empty() {
        return Err(DomainError::Validation(
            "Link URL must include a valid host".to_string(),
        ));
    }

    let trimmed_label = raw_label.trim();
    let label = if trimmed_label.is_empty() {
        suggest_link_label(url).unwrap_or_else(|| "Link".to_string())
    } else {
        trimmed_label.to_string()
    };

    Ok(ItemLink {
        url: url.to_string(),
        label,
    })
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

fn find_query_param<'a>(query: &'a str, target_key_lower: &str) -> Option<&'a str> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k.eq_ignore_ascii_case(target_key_lower) && !v.is_empty() {
            return Some(v);
        }
    }
    None
}

fn parse_digits(s: &str) -> Option<&str> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        Some(s)
    } else {
        None
    }
}

/// Normalizes `<PROJ>-<NUM>` issue keys (e.g. `abc-123` -> `ABC-123`).
fn normalize_issue_key(s: &str) -> Option<String> {
    let (proj, num) = s.split_once('-')?;
    if proj.is_empty() || proj.len() > 20 || num.is_empty() || num.len() > 10 {
        return None;
    }
    let mut chars = proj.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}-{num}", proj.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jira_urls_into_ticket_keys() {
        let cloud = parse_link_url("https://acme.atlassian.net/browse/ABC-123").unwrap();
        assert_eq!(cloud.kind, LinkKind::Jira);
        assert_eq!(cloud.suggested_label, "ABC-123");

        let lowercase = parse_link_url("https://acme.atlassian.net/browse/abc-42").unwrap();
        assert_eq!(lowercase.kind, LinkKind::Jira);
        assert_eq!(lowercase.suggested_label, "ABC-42");

        let board = parse_link_url(
            "https://acme.atlassian.net/jira/software/projects/ABC/boards/5?selectedIssue=ABC-789",
        )
        .unwrap();
        assert_eq!(board.kind, LinkKind::Jira);
        assert_eq!(board.suggested_label, "ABC-789");

        let project_issues =
            parse_link_url("https://jira.internal.corp/projects/CORE/issues/CORE-99").unwrap();
        assert_eq!(project_issues.kind, LinkKind::Jira);
        assert_eq!(project_issues.suggested_label, "CORE-99");

        let hosted_issues =
            parse_link_url("https://acme.atlassian.net/issues/PLAT-101").unwrap();
        assert_eq!(hosted_issues.kind, LinkKind::Jira);
        assert_eq!(hosted_issues.suggested_label, "PLAT-101");
    }

    #[test]
    fn parses_azure_devops_urls_into_hash_ids() {
        let dev_azdo =
            parse_link_url("https://dev.azure.com/org/proj/_workitems/edit/456").unwrap();
        assert_eq!(dev_azdo.kind, LinkKind::AzureDevOps);
        assert_eq!(dev_azdo.suggested_label, "#456");

        let old_vsts =
            parse_link_url("https://org.visualstudio.com/proj/_workitems/edit/789").unwrap();
        assert_eq!(old_vsts.kind, LinkKind::AzureDevOps);
        assert_eq!(old_vsts.suggested_label, "#789");

        let board_query = parse_link_url(
            "https://dev.azure.com/org/proj/_boards/board/t/Team/Stories/?workitem=321",
        )
        .unwrap();
        assert_eq!(board_query.kind, LinkKind::AzureDevOps);
        assert_eq!(board_query.suggested_label, "#321");

        let query_edit =
            parse_link_url("https://tfs.corp.local/tfs/Default/proj/_queries/edit/654").unwrap();
        assert_eq!(query_edit.kind, LinkKind::AzureDevOps);
        assert_eq!(query_edit.suggested_label, "#654");

        let pr = parse_link_url(
            "https://dev.azure.com/org/proj/_git/repo/pullrequest/987",
        )
        .unwrap();
        assert_eq!(pr.kind, LinkKind::AzureDevOps);
        assert_eq!(pr.suggested_label, "#987");
    }

    #[test]
    fn parses_github_gitlab_linear_bitbucket_and_generic_urls() {
        let gh_issue = parse_link_url("https://github.com/rust-lang/rust/issues/456").unwrap();
        assert_eq!(gh_issue.kind, LinkKind::GitHub);
        assert_eq!(gh_issue.suggested_label, "#456");

        let gh_pr =
            parse_link_url("https://github.com/org/repo/pull/1234/files#diff-abc").unwrap();
        assert_eq!(gh_pr.kind, LinkKind::GitHub);
        assert_eq!(gh_pr.suggested_label, "#1234");

        let gh_disc = parse_link_url("https://github.com/org/repo/discussions/88").unwrap();
        assert_eq!(gh_disc.kind, LinkKind::GitHub);
        assert_eq!(gh_disc.suggested_label, "#88");

        let gh_ent = parse_link_url("https://github.acme.com/team/app/issues/77").unwrap();
        assert_eq!(gh_ent.kind, LinkKind::GitHub);
        assert_eq!(gh_ent.suggested_label, "#77");

        let gl_issue = parse_link_url("https://gitlab.com/group/proj/-/issues/15").unwrap();
        assert_eq!(gl_issue.kind, LinkKind::GitLab);
        assert_eq!(gl_issue.suggested_label, "#15");

        let gl_mr = parse_link_url("https://gitlab.com/group/proj/-/merge_requests/22").unwrap();
        assert_eq!(gl_mr.kind, LinkKind::GitLab);
        assert_eq!(gl_mr.suggested_label, "!22");

        let linear = parse_link_url("https://linear.app/acme/issue/eng-101/fix-bug").unwrap();
        assert_eq!(linear.kind, LinkKind::Linear);
        assert_eq!(linear.suggested_label, "ENG-101");

        let bitbucket =
            parse_link_url("https://bitbucket.org/org/repo/pull-requests/55").unwrap();
        assert_eq!(bitbucket.kind, LinkKind::Generic);
        assert_eq!(bitbucket.suggested_label, "#55");

        let generic = parse_link_url("https://www.notion.so/my-doc-123").unwrap();
        assert_eq!(generic.kind, LinkKind::Generic);
        assert_eq!(generic.suggested_label, "notion.so");

        let bare = parse_link_url("github.com/org/repo/issues/99").unwrap();
        assert_eq!(bare.kind, LinkKind::GitHub);
        assert_eq!(bare.suggested_label, "#99");
    }

    #[test]
    fn rejects_invalid_schemes_and_validates_item_links() {
        assert_eq!(parse_link_url(""), None);
        assert_eq!(parse_link_url("   "), None);
        assert_eq!(parse_link_url("javascript:alert(1)"), None);
        assert_eq!(parse_link_url("file:///etc/passwd"), None);
        assert_eq!(parse_link_url("ftp://example.com/file"), None);
        assert_eq!(parse_link_url("not a url"), None);

        assert!(validate_item_link("", "ABC").is_err());
        assert!(validate_item_link("https://example.com/a b", "ABC").is_err());
        assert!(validate_item_link("ftp://example.com/a", "ABC").is_err());
        assert!(validate_item_link("github.com/org/repo/issues/1", "").is_err());
        assert!(validate_item_link("https://", "").is_err());

        let auto = validate_item_link("  https://acme.atlassian.net/browse/abc-123  ", "  ").unwrap();
        assert_eq!(auto.url, "https://acme.atlassian.net/browse/abc-123");
        assert_eq!(auto.label, "ABC-123");

        let custom = validate_item_link("https://github.com/org/repo/issues/12", "Tracker").unwrap();
        assert_eq!(custom.label, "Tracker");
    }
}
