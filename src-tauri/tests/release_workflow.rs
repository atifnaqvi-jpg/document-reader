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

/// The `if:` of the `build-windows` job, if it has one. A job-level condition is invisible to
/// `upload_condition`: the upload step is still written without an `if:`, it just never runs.
fn build_job_condition(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_end() == "  build-windows:")
        .expect("the workflow has no `build-windows` job");
    for line in &lines[start + 1..] {
        if line.starts_with("  ") && !line.starts_with("   ") && !line.trim().is_empty() {
            break;
        }
        if let Some(rest) = line.trim_start().strip_prefix("if:") {
            if line.starts_with("    ") && !line.starts_with("     ") {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// The `if:` of the step named `Upload installer artifact`, if it has one. Unlike the attach
/// step this must be `None`: a dispatch run is only build-only because this upload runs.
fn upload_condition(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains("name: Upload installer artifact"))
        .expect("the workflow has no step named `Upload installer artifact`");
    for line in &lines[start + 1..] {
        let item = line.trim();
        if let Some(rest) = item.strip_prefix("if:") {
            return Some(rest.trim().to_string());
        }
        if line.starts_with("      - ") {
            break;
        }
    }
    None
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

/// The build-only dispatch path needs the job to run at all. A job-level `if:` would leave the
/// upload step looking unconditional in the text while never executing it.
#[test]
fn the_windows_build_job_is_unconditional() {
    assert!(
        build_job_condition(&workflow()).is_none(),
        "the build-windows job has an `if:` condition, so a workflow_dispatch run would build \
         no installer; the upload step's own condition would still read as clean"
    );
}

/// The dispatch run is the only way to get an installer without publishing. That property
/// lives entirely in the upload step running unconditionally, and the attach guard cannot
/// see it: an `if:` added here would make every dispatch produce no artifact while
/// `a_dispatch_run_on_a_branch_does_not_publish_a_release` stayed green.
#[test]
fn the_installer_upload_step_is_unconditional() {
    assert!(
        upload_condition(&workflow()).is_none(),
        "the upload step has an `if:` condition, so a workflow_dispatch run would upload no \
         installer and the build-only path would silently disappear"
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

/// The publish step must say `prerelease: false`, and say it explicitly.
///
/// `softprops/action-gh-release` treats a missing key as `false`, so for 1.0.0 the omission
/// happens to produce the right outcome — but for the wrong reason, and by accident. Stating
/// it explicitly is what makes the intent checkable: a future phase that wants a pre-release
/// has to change the key, and this guard has to move with it.
///
/// History matters here. Up to 0.9.0 this guard required `true`, for the opposite reason:
/// every phase before 1.0.0 is intermediate by ruling Q7, so publishing a stable release would
/// have misrepresented the build. 1.0.0 completes text editing, which is what the flip records.
/// The flag and this guard move in the same commit — one asserting `false` while the other
/// demanded `true` is a tree that contradicts itself.
///
/// Deliberately not a check for "the string appears anywhere": it must be a key at the
/// `with:` depth of the publish step, because a `prerelease: ...` left in a comment or
/// attached to a different step would satisfy a substring search while changing nothing.
///
/// Anchored on the `uses:` line, not the `- ` line: the step's dash carries `name:` and the
/// action is on a following line, which is how both this workflow and every readable one is
/// written.
fn publish_input(text: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let uses = lines.iter().position(|line| {
        let code = line.split('#').next().unwrap_or("").trim();
        code.starts_with("uses:") && code.contains("softprops/action-gh-release")
    })?;
    let uses_indent = lines[uses].len() - lines[uses].trim_start().len();

    let mut with_indent: Option<usize> = None;
    for line in &lines[uses + 1..] {
        let code = line.split('#').next().unwrap_or("");
        if code.trim().is_empty() {
            continue;
        }
        let indent = code.len() - code.trim_start().len();
        let item = code.trim();

        if indent <= uses_indent {
            // Siblings of `uses:` live at the same depth; a new step starts lower.
            if item.starts_with("- ") || indent < uses_indent {
                break;
            }
            if item.trim_end_matches(':') == "with" {
                with_indent = Some(indent);
                continue;
            }
            with_indent = None;
            continue;
        }
        if let Some(w) = with_indent {
            if indent <= w {
                with_indent = None;
            } else if let Some(value) = item.strip_prefix(&format!("{key}:")) {
                return Some(unquote(value));
            }
        }
    }
    None
}

#[test]
fn a_tag_publishes_a_full_release_since_1_0_0() {
    let text = workflow();
    assert!(
        text.contains("softprops/action-gh-release"),
        "the publish step is gone; this guard would otherwise pass vacuously"
    );
    assert_eq!(
        publish_input(&text, "prerelease").as_deref(),
        Some("false"),
        "the publish step must set `prerelease: false` explicitly. 1.0.0 completes text \
         editing, so its tag publishes as a full release; leaving this at `true` marks the \
         shipped release as intermediate, and omitting the key states it by accident rather \
         than on purpose"
    );
}

/// The published release body must name the version this tag actually publishes.
///
/// v1.0.1 published with `**Document Reader 1.0.0** — line-level reflow` as its headline. The
/// text was hardcoded when 1.0.0 was the shipping version and nothing read it again, so a tag
/// three phases later described the wrong product: a user arriving from the Releases page was
/// told this was a text-editing release when it was an annotations release. The asset, the tag
/// and the notes were all correct; only the sentence humans actually read was wrong.
///
/// A hardcoded string cannot be right for more than one version, so this asserts the body
/// carries the version in `Cargo.toml` — the same carrier `release_version.rs` holds the other
/// six to. It reads the body out of the publish step's `with:` block via `publish_input`, which
/// already skips comments, so a version mentioned in the explanatory comment above the block
/// cannot satisfy it.
#[test]
fn the_release_body_names_the_version_being_published() {
    let text = workflow();
    let body = publish_body(&text).expect(
        "the publish step must carry a `body:`; without one GitHub publishes an empty page",
    );

    let version = publish_version();
    assert!(
        body.contains(&version),
        "the release body must name {version}, the version in Cargo.toml. It says: {body:?}. \
         A hardcoded headline published v1.0.1 under 1.0.0's description."
    );
}

/// The body must not still advertise the phase 1.0.0 closed, unless 1.0.0 is what ships.
///
/// The version check above is satisfied by a body naming both versions, which is exactly what
/// a careless edit produces. This is the other half: the phrase 1.0.1 wrongly shipped with must
/// be absent while the tree is past 1.0.0.
#[test]
fn the_release_body_does_not_advertise_a_previous_phases_subject() {
    let body = publish_body(&workflow()).expect("the publish step must carry a `body:`");
    if publish_version() == "1.0.0" {
        return;
    }
    assert!(
        !body.contains("line-level reflow"),
        "the release body still calls this a line-level reflow release, which was 1.0.0's \
         subject. 1.0.1 published that way by accident; it must not happen again."
    );
}

/// Reads `version` from `[package]` in `Cargo.toml`, the same source `release_version.rs` uses
/// to hold all six carriers together.
fn publish_version() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(rest) = line.strip_prefix("version") {
                return rest
                    .trim_start_matches([' ', '='])
                    .trim_matches('"')
                    .to_string();
            }
        }
    }
    panic!("no `version` key under `[package]` in Cargo.toml");
}

/// The publish step's `body:` **as text**, handling the block scalar it is written as.
///
/// `publish_input` returns the raw value on the key's line, which for `body: |` is just `"|"` —
/// the content is in the indented lines below. Comparing a version against `"|"` would fail for
/// every possible version, so this needs its own reader rather than a loosened `publish_input`.
///
/// The block ends at the first non-blank line indented no deeper than the key. Blank lines are
/// kept because the body relies on them for paragraph breaks; trailing blank lines are dropped
/// so the returned text compares cleanly.
fn publish_body(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let uses = lines.iter().position(|line| {
        let code = line.split('#').next().unwrap_or("").trim();
        code.starts_with("uses:") && code.contains("softprops/action-gh-release")
    })?;
    let uses_indent = lines[uses].len() - lines[uses].trim_start().len();

    let mut with_indent: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(uses + 1) {
        let code = line.split('#').next().unwrap_or("");
        if code.trim().is_empty() {
            continue;
        }
        let indent = code.len() - code.trim_start().len();
        let item = code.trim();
        if indent <= uses_indent {
            if item.starts_with("- ") || indent < uses_indent {
                break;
            }
            if item.trim_end_matches(':') == "with" {
                with_indent = Some(indent);
                continue;
            }
            with_indent = None;
            continue;
        }
        let Some(w) = with_indent else { continue };
        if indent <= w {
            with_indent = None;
            continue;
        }
        let Some(rest) = item.strip_prefix("body:") else {
            continue;
        };
        let rest = rest.trim();
        if !(rest == "|" || rest == ">") {
            // Inline body; nothing to unindent.
            return Some(rest.to_string());
        }
        // Block scalar: take every following line indented deeper than the key.
        let mut body: Vec<&str> = Vec::new();
        for follow in &lines[i + 1..] {
            if follow.trim().is_empty() {
                body.push("");
                continue;
            }
            let ind = follow.len() - follow.trim_start().len();
            if ind <= indent {
                break;
            }
            body.push(follow.trim());
        }
        while body.last() == Some(&"") {
            body.pop();
        }
        return Some(body.join("\n"));
    }
    None
}

/// Guard the parser, so a matcher that finds nothing cannot make the test above pass by
/// reading a `None` that looks like an expected absence.
#[test]
fn the_publish_input_reader_finds_only_the_publish_steps_keys() {
    let sample = "\
jobs:
  build:
    steps:
      - uses: actions/checkout@v4
      - name: Attach installer to release
        uses: softprops/action-gh-release@v2
        with:
          files: dist/*.exe
          prerelease: true
      - name: Later step
        with:
          prerelease: false
";
    assert_eq!(publish_input(sample, "prerelease").as_deref(), Some("true"));
    assert_eq!(
        publish_input(sample, "files").as_deref(),
        Some("dist/*.exe")
    );
    assert_eq!(publish_input(sample, "missing"), None);

    let omitted = "\
      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*.exe
";
    assert_eq!(
        publish_input(omitted, "prerelease"),
        None,
        "an omitted key must read as None, which is what separates it from `false` in the \
         caller's assertion"
    );

    // A commented-out key is not a key.
    let commented = "\
      - uses: softprops/action-gh-release@v2
        with:
          # prerelease: true
          files: dist/*.exe
";
    assert_eq!(publish_input(commented, "prerelease"), None);
}
