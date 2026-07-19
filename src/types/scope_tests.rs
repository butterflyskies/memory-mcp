use super::*;

fn path(s: &str) -> Scope {
    Scope::Path(ScopePath::new(s).unwrap())
}

fn subtree(s: &str) -> ScopeFilter {
    ScopeFilter::Subtree(ScopePath::new(s).unwrap())
}

// --- ScopePath validation ---

#[test]
fn validate_scope_path_rejects_traversal() {
    assert!(ScopePath::validate("../evil").is_err());
    assert!(ScopePath::validate("foo/../bar").is_err());
    assert!(ScopePath::validate("..").is_err());
}

#[test]
fn validate_scope_path_rejects_absolute() {
    assert!(ScopePath::validate("/absolute").is_err());
}

#[test]
fn validate_scope_path_rejects_null_bytes() {
    assert!(ScopePath::validate("foo\0bar").is_err());
}

#[test]
fn validate_scope_path_rejects_empty() {
    assert!(ScopePath::validate("").is_err());
}

#[test]
fn validate_scope_path_rejects_empty_component() {
    assert!(ScopePath::validate("foo//bar").is_err());
}

#[test]
fn validate_scope_path_rejects_too_many_segments() {
    let s = "a/b/c/d/e/f/g/h/i/j/k";
    assert!(ScopePath::validate(s).is_err());
}

#[test]
fn validate_scope_path_accepts_max_depth() {
    let s = "a/b/c/d/e/f/g/h/i/j";
    assert!(ScopePath::validate(s).is_ok());
}

#[test]
fn validate_scope_path_rejects_dot_prefix() {
    assert!(ScopePath::validate(".hidden").is_err());
    assert!(ScopePath::validate("foo/.config").is_err());
}

// --- Scope ---

#[test]
fn scope_dir_prefix() {
    assert_eq!(Scope::Root.dir_prefix(), "global");
    assert_eq!(path("foo").dir_prefix(), "projects/foo");
    assert_eq!(path("org/team").dir_prefix(), "projects/org/team");
}

#[test]
fn scope_from_str_global() {
    assert_eq!("global".parse::<Scope>().unwrap(), Scope::Root);
}

#[test]
fn scope_from_str_project_prefix_rejected() {
    assert!("project:my-proj".parse::<Scope>().is_err());
}

#[test]
fn scope_from_str_bare_path() {
    assert_eq!("my-project".parse::<Scope>().unwrap(), path("my-project"));
    assert_eq!("org/team".parse::<Scope>().unwrap(), path("org/team"));
}

#[test]
fn scope_from_str_hierarchical_path() {
    assert_eq!(
        "org/team/project".parse::<Scope>().unwrap(),
        path("org/team/project")
    );
}

#[test]
fn scope_from_str_empty_project_name_fails() {
    assert!("project:".parse::<Scope>().is_err());
}

#[test]
fn scope_from_str_project_traversal_fails() {
    assert!("project:../../etc".parse::<Scope>().is_err());
}

#[test]
fn scope_display() {
    assert_eq!(Scope::Root.to_string(), "global");
    assert_eq!(path("my-project").to_string(), "my-project");
    assert_eq!(path("org/team").to_string(), "org/team");
}

// --- Scope serde ---

#[test]
fn scope_serde_new_variants() {
    let root = Scope::Root;
    let json = serde_json::to_string(&root).unwrap();
    assert!(
        json.contains("\"Root\""),
        "serialized Root should use 'Root': {json}"
    );

    let path_scope = path("foo");
    let json = serde_json::to_string(&path_scope).unwrap();
    assert!(
        json.contains("\"Path\""),
        "serialized Path should use 'Path': {json}"
    );
    assert!(
        json.contains("\"foo\""),
        "serialized Path should contain name: {json}"
    );
}

#[test]
fn scope_serde_round_trip_root() {
    let original = Scope::Root;
    let json = serde_json::to_string(&original).unwrap();
    let parsed: Scope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, original);
}

#[test]
fn scope_serde_round_trip_path() {
    let original = path("org/team");
    let json = serde_json::to_string(&original).unwrap();
    let parsed: Scope = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, original);
}

#[test]
fn scope_serde_legacy_global_deserializes_to_root() {
    let json = r#"{"type": "Global"}"#;
    let parsed: Scope = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, Scope::Root);
}

#[test]
fn scope_serde_legacy_project_deserializes_to_path() {
    let json = r#"{"type": "Project", "name": "foo"}"#;
    let parsed: Scope = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, path("foo"));
}

#[test]
fn scope_serde_rejects_traversal_in_wire_format() {
    let json = r#"{"type":"Path","name":"../evil"}"#;
    assert!(serde_json::from_str::<Scope>(json).is_err());
}

// --- Scope::parse_or_default ---

#[test]
fn test_parse_scope_none_defaults_root() {
    assert_eq!(Scope::parse_or_default(None).unwrap(), Scope::Root);
}

#[test]
fn test_parse_scope_some_global() {
    assert_eq!(
        Scope::parse_or_default(Some("global")).unwrap(),
        Scope::Root
    );
}

#[test]
fn test_parse_scope_project_prefix_rejected() {
    assert!(Scope::parse_or_default(Some("project:my-proj")).is_err());
}

#[test]
fn test_parse_scope_some_bare_path() {
    assert_eq!(
        Scope::parse_or_default(Some("org/team")).unwrap(),
        path("org/team")
    );
}

// --- ScopeFilter::parse_or_default / FromStr ---

#[test]
fn scope_filter_none_defaults_to_root_only() {
    assert_eq!(
        ScopeFilter::parse_or_default(None).unwrap(),
        ScopeFilter::RootOnly
    );
}

#[test]
fn scope_filter_global_returns_root_only() {
    assert_eq!(
        ScopeFilter::parse_or_default(Some("global")).unwrap(),
        ScopeFilter::RootOnly
    );
}

#[test]
fn scope_filter_project_prefix_rejected() {
    assert!(ScopeFilter::parse_or_default(Some("project:my-proj")).is_err());
}

#[test]
fn scope_filter_bare_path_returns_subtree() {
    assert_eq!(
        ScopeFilter::parse_or_default(Some("org/team")).unwrap(),
        subtree("org/team"),
    );
}

#[test]
fn scope_filter_all_returns_all() {
    assert_eq!(
        ScopeFilter::parse_or_default(Some("all")).unwrap(),
        ScopeFilter::All
    );
}

#[test]
fn scope_filter_from_str_global() {
    assert_eq!(
        "global".parse::<ScopeFilter>().unwrap(),
        ScopeFilter::RootOnly
    );
}

#[test]
fn scope_filter_from_str_all() {
    assert_eq!("all".parse::<ScopeFilter>().unwrap(), ScopeFilter::All);
}

#[test]
fn scope_filter_from_str_bare_path() {
    assert_eq!(
        "org/team".parse::<ScopeFilter>().unwrap(),
        subtree("org/team")
    );
}

// --- Subtree matching ---

#[test]
fn scope_filter_subtree_matches_root_path_and_children() {
    assert!(scope_path_matches("eng", "eng"), "exact match");
    assert!(scope_path_matches("eng/ml", "eng"), "child path");
    assert!(
        !scope_path_matches("engineering", "eng"),
        "prefix-string should not match"
    );
    assert!(!scope_path_matches("other", "eng"), "unrelated path");
}

#[test]
fn scope_path_matches_empty_prefix_matches_nothing() {
    assert!(!scope_path_matches("anything", ""));
    assert!(!scope_path_matches("a/b/c", ""));
}

// --- Negative parsing tests ---

#[test]
fn scope_parse_rejects_space() {
    assert!(
        "foo bar".parse::<Scope>().is_err(),
        "scope with space should be rejected"
    );
}

#[test]
fn scope_parse_rejects_colon() {
    assert!(
        "foo:bar".parse::<Scope>().is_err(),
        "bare scope with colon should be rejected"
    );
}

#[test]
fn scope_filter_rejects_space() {
    assert!(
        ScopeFilter::parse_or_default(Some("foo bar")).is_err(),
        "scope filter with space should fail"
    );
}

#[test]
fn scope_parse_bare_path_succeeds() {
    assert!(
        "bogus".parse::<Scope>().is_ok(),
        "'bogus' should be a valid bare path scope"
    );
}

// --- validate_branch_name ---

#[test]
fn validate_branch_name_accepts_valid() {
    assert!(validate_branch_name("main").is_ok());
    assert!(validate_branch_name("feature/foo").is_ok());
    assert!(validate_branch_name("release-1.0").is_ok());
    assert!(validate_branch_name("a/b/c").is_ok());
    assert!(validate_branch_name("my-branch_v2").is_ok());
}

#[test]
fn validate_branch_name_rejects_empty() {
    assert!(validate_branch_name("").is_err());
}

#[test]
fn validate_branch_name_rejects_dot_dot() {
    assert!(validate_branch_name("foo..bar").is_err());
    assert!(validate_branch_name("..").is_err());
}

#[test]
fn validate_branch_name_rejects_invalid_chars() {
    for name in &[
        "foo bar", "foo~bar", "foo^bar", "foo:bar", "foo?bar", "foo*bar", "foo[bar", "foo\\bar",
    ] {
        assert!(
            validate_branch_name(name).is_err(),
            "should reject: {}",
            name
        );
    }
}

#[test]
fn validate_branch_name_rejects_invalid_start_end() {
    assert!(validate_branch_name("/foo").is_err());
    assert!(validate_branch_name("foo/").is_err());
    assert!(validate_branch_name(".foo").is_err());
    assert!(validate_branch_name("foo.").is_err());
}

#[test]
fn validate_branch_name_rejects_consecutive_slashes() {
    assert!(validate_branch_name("foo//bar").is_err());
}

/// Names that passed the validator but fail `git check-ref-format`
/// (#293 review, round 4): an accepted-but-git-invalid ref surfaces as a
/// confusing git error at the first push/pull instead of a clean config
/// rejection.
#[test]
fn validate_branch_name_rejects_git_invalid_refs() {
    // `@{` is reflog syntax.
    assert!(validate_branch_name("foo@{bar").is_err());
    // No component may end with `.lock`.
    assert!(validate_branch_name("foo.lock").is_err());
    assert!(validate_branch_name("foo/bar.lock").is_err());
    assert!(validate_branch_name("foo.lock/bar").is_err());
    // No component may start with `.`.
    assert!(validate_branch_name("foo/.hidden").is_err());
    // The single character `@` is reserved.
    assert!(validate_branch_name("@").is_err());

    // Near-misses stay valid, proving the checks match git's rules rather
    // than banning the characters outright.
    assert!(validate_branch_name("foo@bar").is_ok());
    assert!(validate_branch_name("foo.lockx").is_ok());
    assert!(validate_branch_name("foo.hidden/bar").is_ok());
}
