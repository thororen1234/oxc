use crate::{
    FrameworkFlags,
    fixer::{FixKind, FixTypes},
};

mod allow_warn_deny;
mod filter;

pub use allow_warn_deny::AllowWarnDeny;
pub use filter::{InvalidFilterKind, LintFilter, LintFilterKind};

/// Subset of options used directly by the linter.
#[derive(Debug, Default, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct LintOptions {
    pub fix: FixKind,
    pub framework_hints: FrameworkFlags,
    pub report_unused_directive: Option<AllowWarnDeny>,
    // If true, the linter will create "ignore this section / line" fixes for all diagnostics
    pub with_ignore_fixes: bool,
    /// If true, ignore configuration in comments: disable directives, `/* global */` comments,
    /// and rule configuration comments. Same as ESLint's `noInlineConfig`.
    pub no_inline_config: bool,
    /// If true, ignore rule configuration comments only (`/* eslint rule: "off" */`).
    /// Used by the rule tester, which tests a single rule in isolation.
    pub skip_inline_rule_config: bool,
    /// Types of fixes to apply. Fixes of other types are not applied. Defaults to all types.
    pub fix_types: FixTypes,
}
