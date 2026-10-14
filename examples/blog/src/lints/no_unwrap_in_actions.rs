use kiln::lint::*;

/// Actions don't call `.unwrap()` or `.expect(..)`. A panic in a request is a 500 page with
/// no explanation and nothing in the error report about which input caused it; return the
/// error with `?` instead (`Error::NotFound`, `Error::BadRequest(..)`, or the `Result`
/// you already have), so the person sees the right page and the log says why.
#[lint(code = "A0001")]
pub fn no_unwrap_in_actions(file: &File, report: &mut Report) {
    let Some(syntax) = file.syntax().filter(|_| file.is_in("src/actions")) else { return };
    let mut calls = Calls(vec![]);
    calls.visit_file(syntax);
    for call in calls.0 {
        report.on(
            &call.method,
            format!("`.{}()` in an action panics on bad input; return the error with `?`", call.method),
        );
    }
}

struct Calls(Vec<syn::ExprMethodCall>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "unwrap" || call.method == "expect" {
            self.0.push(call.clone());
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}
