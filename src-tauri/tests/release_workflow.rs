//! The release workflow is release infrastructure, so it gets the same guard as the version
//! carriers: a tag that builds nothing — or, worse, a tag that publishes a release nobody
//! asked for — fails silently and is only noticed days later, from a download page.
//!
//! T-4b was filed believing `v*` did not match `version-0.4.4`. It does: GitHub's filter
//! syntax says `*` matches any run of characters, and its own table spells out that `v2*`
//! "matches branch and tag names that start with `v2`". The 0.4.4 evidence agrees twice over:
//! the workflow has no `branches:` trigger, so its `event=push` run can only have come from
//! the tag push, and the installer asset was published, which needs `refs/tags/version-0.4.4`
//! to start with `refs/tags/v`.
//!
//! So the real defect is the opposite one: `v*` is too *wide*. It matches every tag beginning
//! with the letter v, and the attach guard (`refs/tags/v`) publishes a GitHub Release for every
//! one of them, so a stray `verify-ci` tag ships an installer named after itself. Trigger and
//! guard must both name the two conventions this repository actually uses.

use std::path::PathBuf;

fn workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri must have a parent directory")
        .join(".github/workflows/release-windows.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(['"', '\'']).to_string()
}

/// The globs under `on.push.tags`. Read as text: no YAML crate is a dependency here, and the
/// block we need sits at a fixed depth.
fn tag_globs(text: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let (mut top, mut sub, mut leaf) = (String::new(), String::new(), String::new());
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let item = line.trim();
        match indent {
            0 => {
                top = item.trim_end_matches(':').to_string();
                sub.clear();
                leaf.clear();
            }
            2 => {
                sub = item.trim_end_matches(':').to_string();
                leaf.clear();
            }
            4 if top == "on" && sub == "push" => {
                leaf = item.trim_end_matches(':').to_string();
            }
            _ if top == "on" && sub == "push" && leaf == "tags" && item.starts_with("- ") => {
                globs.push(unquote(&item[2..]));
            }
            _ => {}
        }
    }
    globs
}

/// GitHub filter patterns, as far as this workflow needs them: `*` matches any run of
/// characters except `/`, `**` any run including `/`, `[a-z0-9]` one character from the listed
/// ranges, everything else literal. `?` and `!` are rejected loudly rather than guessed at,
/// because a matcher that quietly mis-reads a pattern would make this test agree with a broken
/// glob.
fn glob_matches(pattern: &str, name: &str) -> bool {
    assert!(
        !pattern.contains('?') && !pattern.contains('!') && !pattern.contains('+'),
        "glob_matches does not implement the `?`/`!`/`+` operators used by {pattern:?}"
    );
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();

    fn set_matches(set: &[char], c: char) -> bool {
        let (mut i, mut hit) = (0usize, false);
        while i < set.len() {
            if i + 2 < set.len() && set[i + 1] == '-' {
                hit |= c >= set[i] && c <= set[i + 2];
                i += 3;
            } else {
                hit |= set[i] == c;
                i += 1;
            }
        }
        hit
    }

    /// `*` consumes a run of characters that contains no `/`; `**` consumes anything.
    fn walk(pat: &[char], text: &[char]) -> bool {
        let Some(&head) = pat.first() else {
            return text.is_empty();
        };
        match head {
            '*' => {
                let double = pat.get(1) == Some(&'*');
                let rest = if double { &pat[2..] } else { &pat[1..] };
                let mut taken = 0;
                loop {
                    if walk(rest, &text[taken..]) {
                        return true;
                    }
                    if taken == text.len() || (!double && text[taken] == '/') {
                        return false;
                    }
                    taken += 1;
                }
            }
            '[' => {
                let close = pat
                    .iter()
                    .position(|&c| c == ']')
                    .unwrap_or_else(|| panic!("unclosed `[` in pattern"));
                match text.first() {
                    Some(&c) if set_matches(&pat[1..close], c) => {
                        walk(&pat[close + 1..], &text[1..])
                    }
                    _ => false,
                }
            }
            literal => text.first() == Some(&literal) && walk(&pat[1..], &text[1..]),
        }
    }

    walk(&pat, &text)
}

fn triggers_build(text: &str, tag: &str) -> bool {
    tag_globs(text).iter().any(|glob| glob_matches(glob, tag))
}

/// The `if:` of the step named `Attach installer to release`, or a panic if the step is
/// missing or unconditional (an unconditional attach would publish on a dispatch run).
fn attach_condition(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains("name: Attach installer to release"))
        .expect("the workflow has no step named `Attach installer to release`");
    for line in &lines[start + 1..] {
        let item = line.trim();
        if let Some(rest) = item.strip_prefix("if:") {
            return rest.trim().to_string();
        }
        if line.starts_with("      - ") {
            break;
        }
    }
    panic!("the attach step has no `if:` guard, so a workflow_dispatch run would try to publish");
}

/// Evaluates the condition as a disjunction of `startsWith(github.ref, '<prefix>')` terms.
/// Anything else panics: this test only claims to understand the shape the workflow ships.
fn attach_accepts(condition: &str, git_ref: &str) -> bool {
    let mut accepted = false;
    for term in condition.split("||") {
        let term = term.trim();
        let prefix = term
            .strip_prefix("startsWith(github.ref, ")
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or_else(|| {
                panic!("the attach condition uses a form this test does not evaluate: {term:?}")
            });
        if git_ref.starts_with(&unquote(prefix)) {
            accepted = true;
        }
    }
    accepted
}

/// Does pushing this tag end with a published release?
fn publishes_release(text: &str, tag: &str, condition: &str) -> bool {
    triggers_build(text, tag) && attach_accepts(condition, &format!("refs/tags/{tag}"))
}

/// Both conventions the repository has used, plus the tag the next release will use. Listed
/// rather than read from `git tag`: `actions/checkout` fetches one ref by default, so in CI a
/// `git tag` lookup would be empty and the test would pass over nothing.
const RELEASE_TAGS: [&str; 4] = ["v0.1.0", "v0.1.1", "version-0.4.4", "version-0.5.0"];

/// Tags that begin with a `v` but are not releases: `verify-ci` and `vendor-drop` are what a
/// stray `git tag` invocation produces, `vnext` and `version-notes` what a human writes when
/// they mean "a marker" rather than "ship 0.6.0".
const NON_RELEASE_TAGS: [&str; 4] = ["verify-ci", "vendor-drop", "vnext", "version-notes"];

#[test]
fn the_trigger_accepts_every_release_tag_convention() {
    let text = workflow();
    let globs = tag_globs(&text);
    assert!(
        !globs.is_empty(),
        "no `on.push.tags` globs found in release-windows.yml"
    );

    let missed: Vec<&str> = RELEASE_TAGS
        .iter()
        .copied()
        .filter(|tag| !triggers_build(&text, tag))
        .collect();

    assert!(
        missed.is_empty(),
        "these tags match no trigger glob {globs:?}, so pushing them builds no installer: {missed:?}"
    );
}

/// T-4b, restated: the trigger must name the release conventions, not merely start with the
/// letter their names share.
#[test]
fn a_tag_that_is_not_a_release_does_not_trigger_the_installer() {
    let text = workflow();
    let globs = tag_globs(&text);

    let spurious: Vec<&str> = NON_RELEASE_TAGS
        .iter()
        .copied()
        .filter(|tag| triggers_build(&text, tag))
        .collect();

    assert!(
        spurious.is_empty(),
        "the trigger globs {globs:?} also match non-release tags {spurious:?}, so a stray tag \
         spends a Windows build"
    );
}

/// The invariant that actually protects the release page: a tag either ships an installer or
/// does not touch the release at all. Narrowing the trigger while leaving the attach guard wide
/// would still publish, so this checks the guard on its own terms.
#[test]
fn no_tag_outside_the_release_conventions_publishes_a_release() {
    let text = workflow();
    let condition = attach_condition(&text);

    let spurious: Vec<&str> = NON_RELEASE_TAGS
        .iter()
        .copied()
        .filter(|tag| publishes_release(&text, tag, &condition))
        .collect();

    assert!(
        spurious.is_empty(),
        "`if: {condition}` publishes a GitHub Release for these non-release tags: {spurious:?}"
    );
}

/// The other half of the same invariant: every tag that does build must get its installer
/// attached, or the release page carries a version with no download on it.
#[test]
fn every_tag_that_triggers_the_build_also_gets_the_installer_attached() {
    let text = workflow();
    let condition = attach_condition(&text);

    let unattained: Vec<&str> = RELEASE_TAGS
        .iter()
        .copied()
        .filter(|tag| triggers_build(&text, tag))
        .filter(|tag| !attach_accepts(&condition, &format!("refs/tags/{tag}")))
        .collect();

    assert!(
        unattained.is_empty(),
        "the build runs for these tags but `if: {condition}` never attaches the installer, \
         so the release has no download: {unattained:?}"
    );
}

/// A dispatch run is documented as producing an artifact without publishing a release.
#[test]
fn a_dispatch_run_on_a_branch_does_not_publish_a_release() {
    let condition = attach_condition(&workflow());
    assert!(
        !attach_accepts(&condition, "refs/heads/p2-windows-test"),
        "`if: {condition}` publishes on a branch ref, but only tag runs should create a release"
    );
}

/// Guard the matcher itself. Without this, a matcher that answered `false` to everything would
/// make the non-release test pass by accident, and one that answered `true` would pass it while
/// proving nothing.
#[test]
fn the_glob_matcher_follows_the_documented_semantics() {
    assert!(glob_matches("v*", "v0.1.0"));
    assert!(
        glob_matches("v*", "version-0.4.4"),
        "`*` is bounded by nothing but `/`, which is why the old glob was too wide"
    );
    assert!(!glob_matches("v*", "release-v1"));
    assert!(!glob_matches("v*", "v/1"), "`*` does not cross a slash");
    assert!(glob_matches("version-*", "version-0.4.4"));
    assert!(!glob_matches("version-*", "v0.1.0"));

    assert!(glob_matches("v[0-9]*", "v0.1.0"));
    assert!(!glob_matches("v[0-9]*", "version-0.4.4"));
    assert!(!glob_matches("v[0-9]*", "verify-ci"));
    assert!(glob_matches("version-[0-9]*", "version-0.5.0"));
    assert!(!glob_matches("version-[0-9]*", "version-notes"));
    assert!(glob_matches("[0-9a-z]", "7"));
}
