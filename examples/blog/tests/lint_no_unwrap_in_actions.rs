use blog::lints::no_unwrap_in_actions;
use kiln::lint::check;

#[test]
fn no_unwrap_in_actions_flags_unwrap_and_expect() {
    let source = "fn show() {\n    let a = x.unwrap();\n    let b = y.map(|v| v.expect(\"set\"));\n}\n";
    let found = check(no_unwrap_in_actions, "src/actions/posts/show.rs", source);
    assert_eq!(found.iter().map(|f| f.line).collect::<Vec<_>>(), [Some(2), Some(3)], "{found:?}");
}

#[test]
fn no_unwrap_in_actions_allows_question_marks_and_other_dirs() {
    let found = check(no_unwrap_in_actions, "src/actions/posts/show.rs", "fn show() -> Result { x?; Ok(()) }\n");
    assert!(found.is_empty(), "{found:?}");
    let found = check(no_unwrap_in_actions, "src/ops/sign_up.rs", "fn op() { x.unwrap(); }\n");
    assert!(found.is_empty(), "{found:?}");
}
