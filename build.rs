use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[allow(dead_code)]
#[path = "src/version.rs"]
mod version;

fn git_stdout(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn git_path(rel: &str) -> Option<String> {
    git_stdout(&["rev-parse", "--git-path", rel])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn collect_git_head_tags_and_watch_paths() -> (Vec<String>, BTreeSet<String>) {
    let mut watch_paths = BTreeSet::new();

    for base in ["HEAD", "refs/tags", "packed-refs"] {
        if let Some(p) = git_path(base) {
            watch_paths.insert(p);
        }
    }

    if let Some(head_ref) = git_stdout(&["symbolic-ref", "-q", "HEAD"]) {
        let head_ref = head_ref.trim();
        if !head_ref.is_empty() {
            if let Some(p) = git_path(head_ref) {
                watch_paths.insert(p);
            }
        }
    }

    let tags: Vec<String> = git_stdout(&["tag", "--points-at", "HEAD"])
        .map(|out| {
            out.lines()
                .map(|line| line.trim_end_matches('\r').to_string())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default();

    for tag in &tags {
        if let Some(p) = git_path(&format!("refs/tags/{tag}")) {
            watch_paths.insert(p);
        }
    }

    (tags, watch_paths)
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/version.rs");
    println!("cargo::rerun-if-env-changed=PROTRACK_VERSION");
    println!("cargo::rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo::rerun-if-env-changed=GITHUB_REF_NAME");

    let explicit_override = std::env::var("PROTRACK_VERSION").ok();
    let github_ref_type = std::env::var("GITHUB_REF_TYPE").ok();
    let github_ref_name = std::env::var("GITHUB_REF_NAME").ok();
    let fallback_version = env!("CARGO_PKG_VERSION");

    let has_explicit = explicit_override
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let has_ci_ref = github_ref_type
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());

    let git_head_tags = if !has_explicit && !has_ci_ref {
        let (tags, watch_paths) = collect_git_head_tags_and_watch_paths();
        for path in watch_paths {
            if Path::new(&path).exists() {
                println!("cargo::rerun-if-changed={path}");
            }
        }
        tags
    } else {
        Vec::new()
    };

    let resolved = version::resolve_app_version(
        explicit_override.as_deref(),
        github_ref_type.as_deref(),
        github_ref_name.as_deref(),
        &git_head_tags,
        fallback_version,
    );

    println!("cargo::rustc-env=PROTRACK_APP_VERSION={resolved}");
}
