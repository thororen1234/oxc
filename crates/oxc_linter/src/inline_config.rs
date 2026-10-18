//! Rule configuration comments, e.g. `/* eslint no-console: "off", quotes: ["error", "double"] */`.
//!
//! As in ESLint, a configuration comment applies to the whole file it's in, and when several comments
//! configure the same rule, the last one wins. Setting only a severity keeps the rule's configured options.

use std::sync::Arc;

use oxc_ast::Comment;
use oxc_diagnostics::OxcDiagnostic;
use oxc_span::Span;
use smallvec::SmallVec;

use crate::{
    AllowWarnDeny, LintPlugins, Message, PossibleFixes,
    config::{
        ConfigStore,
        rules::{parse_rule_key, parse_rule_value, transform_rule_and_plugin_name},
    },
    context::ContextHost,
    external_plugin_store::{ExternalOptionsId, ExternalRuleId},
    rules::{RULES, RuleEnum},
};

type Rules = Arc<[(RuleEnum, AllowWarnDeny)]>;
type ExternalRules = Arc<[(ExternalRuleId, ExternalOptionsId, AllowWarnDeny)]>;

/// One rule's configuration from a configuration comment.
struct InlineRuleConfig {
    key: String,
    value: serde_json::Value,
    /// Span of the comment, relative to the start of the file.
    comment_span: Span,
}

/// Apply rule configuration comments in the file to `rules` and `external_rules`.
///
/// Returns `None` if the file has no configuration comments, so the common case doesn't copy the rules.
/// Otherwise returns the rules for this file, and diagnostics for invalid configuration comments.
pub fn apply_inline_rule_configs(
    ctx_host: &ContextHost<'_>,
    config_store: &ConfigStore,
    rules: &Rules,
    external_rules: &ExternalRules,
) -> Option<(Rules, ExternalRules, Vec<Message>)> {
    let mut diagnostics = vec![];
    let mut configs = vec![];
    for sub_host in ctx_host.sub_hosts() {
        let source_text = sub_host.source_text();
        let offset = sub_host.source_text_offset();
        for comment in sub_host.semantic().comments().iter().filter(|comment| comment.is_block()) {
            let Some(result) = parse_comment(comment, source_text) else {
                continue;
            };
            let comment_span = Span::new(comment.span.start + offset, comment.span.end + offset);
            match result {
                Ok(entries) => {
                    configs.extend(entries.into_iter().map(|(key, value)| InlineRuleConfig {
                        key,
                        value,
                        comment_span,
                    }));
                }
                Err(error) => diagnostics.push(invalid_comment(&error, comment_span)),
            }
        }
    }

    if configs.is_empty() && diagnostics.is_empty() {
        return None;
    }

    let mut rules = rules.to_vec();
    let mut external_rules = external_rules.to_vec();
    for config in configs {
        if let Err(error) =
            apply_rule_config(&config, config_store, &mut rules, &mut external_rules)
        {
            diagnostics.push(error);
        }
    }

    Some((rules.into(), external_rules.into(), diagnostics))
}

fn apply_rule_config(
    config: &InlineRuleConfig,
    config_store: &ConfigStore,
    rules: &mut Vec<(RuleEnum, AllowWarnDeny)>,
    external_rules: &mut Vec<(ExternalRuleId, ExternalOptionsId, AllowWarnDeny)>,
) -> Result<(), Message> {
    let InlineRuleConfig { key, value, comment_span } = config;
    let error = |message: String| {
        Message::new(OxcDiagnostic::error(message).with_label(*comment_span), PossibleFixes::None)
    };

    let (severity, options) = parse_rule_value(value.clone()).map_err(|err| {
        error(format!("Inline configuration for rule \"{key}\" is invalid: {err}"))
    })?;

    let (plugin_name, rule_name) = parse_rule_key(key);
    let (rule_name, plugin_name) = transform_rule_and_plugin_name(&rule_name, &plugin_name);

    if LintPlugins::try_from(plugin_name).is_err() {
        return apply_external_rule_config(
            config_store,
            plugin_name,
            rule_name,
            severity,
            &options,
            external_rules,
        )
        .map_err(error);
    }

    let index =
        rules.iter().position(|(r, _)| r.name() == rule_name && r.plugin_name() == plugin_name);

    if severity == AllowWarnDeny::Allow {
        if let Some(index) = index {
            rules.remove(index);
        }
        return Ok(());
    }

    // Severity only: keep the configured options, if the rule is configured.
    if options.is_empty()
        && let Some(index) = index
    {
        rules[index].1 = severity;
        return Ok(());
    }

    let Some(template) =
        RULES.iter().find(|r| r.name() == rule_name && r.plugin_name() == plugin_name)
    else {
        return Err(error(format!("Definition for rule \"{key}\" was not found.")));
    };
    if !options.is_empty() && !template.has_config() {
        return Err(error(format!(
            "Inline configuration for rule \"{key}\" is invalid: This rule does not accept configuration options."
        )));
    }
    let options = if options.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Array(options.into_vec())
    };
    let rule = template.from_configuration(options).map_err(|err| {
        error(format!("Inline configuration for rule \"{key}\" is invalid: {err}"))
    })?;

    match index {
        Some(index) => rules[index] = (rule, severity),
        None => rules.push((rule, severity)),
    }
    Ok(())
}

fn apply_external_rule_config(
    config_store: &ConfigStore,
    plugin_name: &str,
    rule_name: &str,
    severity: AllowWarnDeny,
    options: &SmallVec<[serde_json::Value; 1]>,
    external_rules: &mut Vec<(ExternalRuleId, ExternalOptionsId, AllowWarnDeny)>,
) -> Result<(), String> {
    let full_name = format!("{plugin_name}/{rule_name}");
    let Ok(rule_id) = config_store.external_plugin_store().lookup_rule_id(plugin_name, rule_name)
    else {
        // Turning off a rule from a plugin oxlint doesn't have is harmless, and common in code
        // migrated from ESLint, so only report unknown rules which would be enabled.
        if severity == AllowWarnDeny::Allow {
            return Ok(());
        }
        return Err(format!("Definition for rule \"{full_name}\" was not found."));
    };
    let index = external_rules.iter().position(|(id, _, _)| *id == rule_id);

    if severity == AllowWarnDeny::Allow {
        if let Some(index) = index {
            external_rules.remove(index);
        }
        return Ok(());
    }

    // Options of JS plugin rules are sent to JS before linting starts, so they can't be changed per file.
    if !options.is_empty() {
        return Err(format!(
            "Inline configuration for rule \"{full_name}\" is invalid: Options for JS plugin rules can't be set in configuration comments."
        ));
    }

    match index {
        Some(index) => external_rules[index].2 = severity,
        None => external_rules.push((rule_id, ExternalOptionsId::NONE, severity)),
    }
    Ok(())
}

fn invalid_comment(error: &str, span: Span) -> Message {
    Message::new(
        OxcDiagnostic::error(format!("Failed to parse configuration comment: {error}"))
            .with_label(span),
        PossibleFixes::None,
    )
}

/// Parse a `/* eslint ... */` or `/* oxlint ... */` configuration comment.
///
/// Returns `None` if the comment isn't a configuration comment.
fn parse_comment(
    comment: &Comment,
    source_text: &str,
) -> Option<Result<Vec<(String, serde_json::Value)>, String>> {
    parse_comment_text(comment.content_span().source_text(source_text))
}

fn parse_comment_text(text: &str) -> Option<Result<Vec<(String, serde_json::Value)>, String>> {
    let text = text.trim_start();
    let rest = text.strip_prefix("eslint").or_else(|| text.strip_prefix("oxlint"))?;
    // Must be followed by whitespace, so `eslint-disable` etc. aren't matched.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = strip_description(rest).trim();
    if rest.is_empty() {
        return Some(Ok(vec![]));
    }

    let json = format!("{{{}}}", json_like_to_json(rest));
    Some(match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json) {
        Ok(map) => Ok(map.into_iter().collect()),
        Err(err) => Err(err.to_string()),
    })
}

/// Remove a description, i.e. everything from the first `--` (2 or more dashes) surrounded by whitespace.
fn strip_description(text: &str) -> &str {
    let bytes = text.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_whitespace() {
            continue;
        }
        let dashes = bytes[index + 1..].iter().take_while(|&&b| b == b'-').count();
        if dashes >= 2 && bytes.get(index + 1 + dashes).is_some_and(u8::is_ascii_whitespace) {
            return &text[..index];
        }
    }
    text
}

/// Convert ESLint's relaxed configuration syntax to JSON, without the surrounding `{}`.
///
/// Supports what ESLint accepts: unquoted keys and string values (`quotes: [error, double]`),
/// single-quoted strings, and entries separated by whitespace instead of commas.
fn json_like_to_json(text: &str) -> String {
    #[derive(PartialEq, Eq, Clone, Copy)]
    enum Last {
        /// Start of an object or array, a `,` or a `:`.
        Separator,
        /// End of a value.
        Value,
    }

    let mut output = String::with_capacity(text.len() + 16);
    let mut last = Last::Separator;
    let mut chars = text.char_indices().peekable();

    while let Some((start, c)) = chars.next() {
        let quote = c;
        match c {
            c if c.is_whitespace() => {}
            '{' | '[' => {
                if last == Last::Value {
                    output.push(',');
                }
                output.push(c);
                last = Last::Separator;
            }
            '}' | ']' => {
                output.push(c);
                last = Last::Value;
            }
            ',' | ':' => {
                output.push(c);
                last = Last::Separator;
            }
            '"' | '\'' => {
                if last == Last::Value {
                    output.push(',');
                }
                output.push('"');
                let mut escaped = false;
                for (_, c) in chars.by_ref() {
                    if escaped {
                        // `\'` is not a valid JSON escape.
                        if c != '\'' {
                            output.push('\\');
                        }
                        output.push(c);
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == quote {
                        break;
                    } else if c == '"' {
                        output.push_str("\\\"");
                    } else {
                        output.push(c);
                    }
                }
                output.push('"');
                last = Last::Value;
            }
            _ => {
                // Unquoted word: a key, number, `true`/`false`/`null`, or a string.
                let mut end = start + c.len_utf8();
                while let Some(&(index, c)) = chars.peek() {
                    if c.is_whitespace()
                        || matches!(c, ',' | ':' | '[' | ']' | '{' | '}' | '"' | '\'')
                    {
                        break;
                    }
                    end = index + c.len_utf8();
                    chars.next();
                }
                let word = &text[start..end];
                if last == Last::Value {
                    output.push(',');
                }
                let is_key =
                    chars.clone().find(|(_, c)| !c.is_whitespace()).is_some_and(|(_, c)| c == ':');
                let is_literal = !is_key
                    && (matches!(word, "true" | "false" | "null") || word.parse::<f64>().is_ok());
                if is_literal {
                    output.push_str(word);
                } else {
                    output.push('"');
                    // Words can't contain quotes, as they end at a quote.
                    output.push_str(word);
                    output.push('"');
                }
                last = Last::Value;
            }
        }
    }
    output
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::*;

    fn parse(text: &str) -> Option<Result<Vec<(String, serde_json::Value)>, String>> {
        parse_comment_text(text)
    }

    #[test]
    fn test_parse_comment_text() {
        assert_eq!(
            parse("eslint no-console: \"off\""),
            Some(Ok(vec![("no-console".into(), json!("off"))]))
        );
        assert_eq!(
            parse(" eslint quotes: [\"error\", \"double\"], curly: 2 "),
            Some(Ok(vec![
                ("quotes".into(), json!(["error", "double"])),
                ("curly".into(), json!(2)),
            ]))
        );
        // Relaxed syntax: unquoted values, single quotes, no commas, object options.
        assert_eq!(
            parse(
                "eslint eqeqeq: off curly: [error, 'multi'] no-magic-numbers: [warn, {ignore: [1, -1], enforceConst: true}]"
            ),
            Some(Ok(vec![
                ("eqeqeq".into(), json!("off")),
                ("curly".into(), json!(["error", "multi"])),
                (
                    "no-magic-numbers".into(),
                    json!(["warn", {"ignore": [1, -1], "enforceConst": true}])
                ),
            ]))
        );
        assert_eq!(
            parse("oxlint @typescript-eslint/no-explicit-any: warn -- legacy code"),
            Some(Ok(vec![("@typescript-eslint/no-explicit-any".into(), json!("warn"))]))
        );
        assert_eq!(parse("eslint"), None);
        assert_eq!(parse("eslint-disable no-console"), None);
        assert_eq!(parse("eslint-env browser"), None);
        assert_eq!(parse("global foo"), None);
        assert!(matches!(parse("eslint no-console: [off"), Some(Err(_))));
    }
}
