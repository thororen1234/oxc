use std::{borrow, fmt, hash, ops::Deref};

use rustc_hash::FxHashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::Visitor};

/// Add or remove global variables.
///
/// For each global variable, set the corresponding value equal to `"writable"`
/// to allow the variable to be overwritten or `"readonly"` to disallow overwriting.
///
/// Globals can be disabled by setting their value to `"off"`. For example, in
/// an environment where most Es2015 globals are available but `Promise` is unavailable,
/// you might use this config:
///
/// ```json
///
/// {
///     "$schema": "./node_modules/oxlint/configuration_schema.json",
///     "env": {
///         "es6": true
///     },
///     "globals": {
///         "Promise": "off"
///     }
/// }
///
/// ```
///
/// You may also use `"readable"` or `false` to represent `"readonly"`, and
/// `"writeable"` or `true` to represent `"writable"`.
// <https://eslint.org/docs/v8.x/use/configure/language-options#using-configuration-files-1>
#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema, Clone)]
pub struct OxlintGlobals(FxHashMap<String, GlobalValue>);

impl Deref for OxlintGlobals {
    type Target = FxHashMap<String, GlobalValue>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl OxlintGlobals {
    pub fn is_enabled<Q>(&self, name: &Q) -> bool
    where
        String: borrow::Borrow<Q>,
        Q: ?Sized + Eq + hash::Hash,
    {
        self.0.get(name).is_some_and(|value| *value != GlobalValue::Off)
    }

    pub(crate) fn override_globals(&self, globals_to_override: &mut OxlintGlobals) {
        for (env, supported) in self.0.clone() {
            globals_to_override.0.insert(env, supported);
        }
    }

    /// Merge globals declared in `/* global */` and `/* globals */` comments over these globals,
    /// as ESLint does. `comments` are the contents of block comments.
    ///
    /// Returns `None` if there are no such comments, so the common case doesn't clone the config's globals.
    pub(crate) fn with_comment_globals<'c>(
        &self,
        comments: impl IntoIterator<Item = &'c str>,
    ) -> Option<Self> {
        let mut merged: Option<Self> = None;
        for comment in comments {
            let Some(declarations) = parse_global_comment(comment) else {
                continue;
            };
            let merged = merged.get_or_insert_with(|| self.clone());
            merged.0.extend(declarations);
        }
        merged
    }
}

/// Parse the contents of a `/* global a, b: writable */` or `/* globals ... */` comment.
///
/// Follows ESLint's `ConfigCommentParser#parseStringConfig` and `normalizeConfigGlobal`.
/// Declarations with an invalid value are skipped.
fn parse_global_comment(comment: &str) -> Option<Vec<(String, GlobalValue)>> {
    let rest = comment.trim_start().strip_prefix("global")?;
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }

    // Strip a trailing description, e.g. `/* global foo -- provided by the page */`.
    let rest = strip_directive_description(rest);

    let normalized = collapse_whitespace_around_separators(rest);
    let declarations = normalized
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let mut parts = entry.split(':');
            let name = parts.next()?;
            let value = match parts.next() {
                None | Some("false") => GlobalValue::Readonly,
                Some("true") => GlobalValue::Writable,
                Some(value) => GlobalValue::try_from(value).ok()?,
            };
            Some((name.to_string(), value))
        })
        .collect();
    Some(declarations)
}

/// Remove whitespace before and after `:` and `,`. Same as ESLint's `/(?<!\s)\s*([:,])\s*/g` replacement.
fn collapse_whitespace_around_separators(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut pending_whitespace = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            pending_whitespace.push(c);
            continue;
        }
        let after_separator = output.ends_with([':', ',']);
        if !(c == ':' || c == ',' || after_separator) {
            output.push_str(&pending_whitespace);
        }
        pending_whitespace.clear();
        output.push(c);
    }
    output
}

/// Remove the description from a directive comment, i.e. everything from the first
/// `--` (two or more dashes) surrounded by whitespace. Same as ESLint's `/\s-{2,}\s/` split.
fn strip_directive_description(text: &str) -> &str {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GlobalValue {
    Readonly,
    Writable,
    Off,
}

impl GlobalValue {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Readonly => "readonly",
            Self::Writable => "writable",
            Self::Off => "off",
        }
    }
}

impl<'de> Deserialize<'de> for GlobalValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(GlobalValueVisitor)
    }
}

impl From<bool> for GlobalValue {
    #[inline]
    fn from(value: bool) -> Self {
        if value { GlobalValue::Writable } else { GlobalValue::Readonly }
    }
}

impl TryFrom<&str> for GlobalValue {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "readonly" | "readable" => Ok(GlobalValue::Readonly),
            "writable" | "writeable" => Ok(GlobalValue::Writable),
            "off" => Ok(GlobalValue::Off),
            _ => Err("Invalid global value"),
        }
    }
}

impl fmt::Display for GlobalValue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

struct GlobalValueVisitor;
impl Visitor<'_> for GlobalValueVisitor {
    type Value = GlobalValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("'readonly', 'writable', 'off', or a boolean")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        v.try_into().map_err(E::custom)
    }
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::*;

    macro_rules! globals {
        ($($json:tt)+) => {
            OxlintGlobals::deserialize(&json!($($json)+)).unwrap()
        };
    }

    #[test]
    fn test_deserialize_normal() {
        let globals = globals!({
            "foo": "readonly",
            "bar": "writable",
            "baz": "off",
        });
        assert!(globals.is_enabled("foo"));
        assert!(globals.is_enabled("bar"));
        assert!(!globals.is_enabled("baz"));

        assert_eq!(globals.get("foo"), Some(&GlobalValue::Readonly));
        assert_eq!(globals.get("bar"), Some(&GlobalValue::Writable));
        assert_eq!(globals.get("baz"), Some(&GlobalValue::Off));
    }

    #[test]
    fn test_deserialize_legacy_spelling() {
        let globals = globals!({
            "foo": "readable",
            "bar": "writeable",
        });
        assert!(globals.is_enabled("foo"));
        assert!(globals.is_enabled("bar"));
        // Ensure they map to the correct variants
        assert_eq!(globals.get("foo"), Some(&GlobalValue::Readonly));
        assert_eq!(globals.get("bar"), Some(&GlobalValue::Writable));
    }

    #[test]
    fn test_deserialize_bool() {
        let globals = globals!({
            "foo": true,
            "bar": false,
        });
        assert!(globals.is_enabled("foo"));
        assert!(globals.is_enabled("bar"));

        assert_eq!(globals.get("foo"), Some(&GlobalValue::Writable));
        assert_eq!(globals.get("bar"), Some(&GlobalValue::Readonly));
    }

    #[test]
    fn test_parse_global_comment() {
        let parse = |comment| {
            parse_global_comment(comment)
                .map(|declarations| declarations.into_iter().collect::<FxHashMap<_, _>>())
        };
        let expected = |pairs: &[(&str, GlobalValue)]| {
            Some(pairs.iter().map(|(name, value)| ((*name).to_string(), *value)).collect())
        };

        assert_eq!(parse("global a"), expected(&[("a", GlobalValue::Readonly)]));
        assert_eq!(
            parse(" globals a, b:true c : writable,d:false e:off "),
            expected(&[
                ("a", GlobalValue::Readonly),
                ("b", GlobalValue::Writable),
                ("c", GlobalValue::Writable),
                ("d", GlobalValue::Readonly),
                ("e", GlobalValue::Off),
            ])
        );
        assert_eq!(
            parse("global a -- b, c"),
            expected(&[("a", GlobalValue::Readonly)]),
            "description after `--` is ignored"
        );
        assert_eq!(parse("global a--b"), expected(&[("a--b", GlobalValue::Readonly)]));
        assert_eq!(
            parse("global a:, b: nope, c"),
            expected(&[("c", GlobalValue::Readonly)]),
            "invalid values are skipped"
        );
        assert_eq!(parse("global"), expected(&[]));
        assert_eq!(parse("globalfoo a"), None);
        assert_eq!(parse("eslint-disable"), None);
    }

    #[test]
    fn test_with_comment_globals() {
        let config = globals!({ "a": "writable", "b": "readonly" });

        assert!(config.with_comment_globals(["not a directive", " eslint-disable "]).is_none());

        let merged = config.with_comment_globals(["global b: off, c", "globals d:true"]).unwrap();
        assert_eq!(merged.get("a"), Some(&GlobalValue::Writable));
        assert_eq!(merged.get("b"), Some(&GlobalValue::Off));
        assert_eq!(merged.get("c"), Some(&GlobalValue::Readonly));
        assert_eq!(merged.get("d"), Some(&GlobalValue::Writable));
    }

    #[test]
    fn test_override_globals() {
        let mut globals = OxlintGlobals::deserialize(&serde_json::json!({
            "Foo": "writable",
        }))
        .unwrap();
        let override_globals = OxlintGlobals::deserialize(&serde_json::json!({
            "Foo": "off",
        }))
        .unwrap();

        override_globals.override_globals(&mut globals);

        assert!(!globals.is_enabled("Foo"));
    }
}
