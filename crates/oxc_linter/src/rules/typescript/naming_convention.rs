use oxc_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use oxc_macros::declare_oxc_lint;
use oxc_span::Span;

use crate::{context::LintContext, rule::Rule, AstNode};

#[derive(Debug, Error, Diagnostic)]
#[error("typescript-eslint(naming-convention):")]
#[diagnostic(severity(warning), help(""))]
struct NamingConventionDiagnostic(#[label] pub Span);

#[derive(Debug, Default, Clone)]
pub struct NamingConvention;

declare_oxc_lint!(
    /// ### What it does
    /// Enforce naming conventions for everything across a codebase.
    ///
    /// ### Why ?
    /// Enforcing naming conventions helps keep the codebase consistent, and reduces overhead when thinking about how to name a variable. 
    /// Additionally, a well-designed style guide can help communicate intent, such as by enforcing all private properties begin with an _, and all global-level constants are written in UPPER_CASE.
    ///
    /// ### Example
    /// ```javascript
    /// ```
    NamingConvention,
    correctness
);

impl Rule for NamingConvention {
    fn run<'a>(&self, node: &AstNode<'a>, ctx: &LintContext<'a>) {}
}

#[test]
fn test() {
    use crate::tester::Tester;

    let pass = vec![];

    let fail = vec![];

    Tester::new(NamingConvention::NAME, pass, fail).test_and_snapshot();
}
