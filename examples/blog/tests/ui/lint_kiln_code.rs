// The app's rules can't take Kiln's `K` codes, and need the rule's shape.
use kiln::lint::*;

#[lint(code = "K0101")]
fn mine(_file: &File, _report: &mut Report) {}

#[lint(code = "A1")]
fn short(_file: &File, _report: &mut Report) {}

#[lint(code = "A0002")]
fn wrong_shape(_file: &File) {}

fn main() {}
