//! Lenient serde deserializers for tool-input fields.
//!
//! Some providers (notably Claude's tool calling) emit numeric and boolean
//! tool arguments as JSON *strings* — e.g. `{"compactions": "0"}` instead of
//! `{"compactions": 0}` — even when the tool's JSON schema declares the field
//! as `integer`/`boolean`. `serde_json` is strict by default and rejects these
//! with errors like `invalid type: string "0", expected u32`, which causes the
//! whole tool call to fail (see issue #106 for `end_ambient_cycle`).
//!
//! These helpers accept either the native JSON type or a string representation
//! and coerce to the target type, so tool inputs survive that provider quirk.
//! Apply them per-field with `#[serde(deserialize_with = ...)]` on fields whose
//! schema declares a numeric or boolean type.

use serde::{Deserialize, Deserializer, de};
use std::fmt;

struct U32OrString;

impl<'de> de::Visitor<'de> for U32OrString {
    type Value = u32;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a u32 or a string representing a u32")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
        u32::try_from(v).map_err(|_| E::custom(format!("number {v} out of range for u32")))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<u32, E> {
        u32::try_from(v).map_err(|_| E::custom(format!("number {v} out of range for u32")))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<u32, E> {
        if v.fract() == 0.0 && v >= 0.0 && v <= f64::from(u32::MAX) {
            Ok(v as u32)
        } else {
            Err(E::custom(format!("number {v} is not a valid u32")))
        }
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
        let trimmed = v.trim();
        trimmed
            .parse::<u32>()
            .map_err(|_| E::custom(format!("string {trimmed:?} is not a valid u32")))
    }
}

/// Deserialize a `u32` from either a JSON number or a numeric string.
pub fn u32_from_string_or_number<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(U32OrString)
}

/// Deserialize an `Option<u32>` from either a JSON number, a numeric string,
/// or null/missing. Empty strings deserialize to `None`.
pub fn opt_u32_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    // Accept null, missing, number, or string.
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| de::Error::custom(format!("string {:?} is not a valid u32", s.trim()))),
        Some(serde_json::Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                u32::try_from(u)
                    .map(Some)
                    .map_err(|_| de::Error::custom(format!("number {u} out of range for u32")))
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f >= 0.0 && f <= f64::from(u32::MAX) {
                    Ok(Some(f as u32))
                } else {
                    Err(de::Error::custom(format!("number {f} is not a valid u32")))
                }
            } else {
                Err(de::Error::custom("number is not a valid u32"))
            }
        }
        Some(other) => Err(de::Error::custom(format!(
            "expected u32 or numeric string, got {other}"
        ))),
    }
}

/// Deserialize an `Option<u64>` from a JSON number, a numeric string, or
/// null/missing. Empty strings deserialize to `None`.
///
/// Same provider quirk as the `u32` helpers: models emit `"600"` for a field
/// whose schema says `integer`, and strict serde rejects the entire tool call
/// with `invalid type: string "600", expected u64`. The model gets no partial
/// credit, so a whole turn is wasted on a formatting detail.
pub fn opt_u64_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| de::Error::custom(format!("string {:?} is not a valid u64", s.trim()))),
        Some(serde_json::Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(Some(u))
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f >= 0.0 && f <= u64::MAX as f64 {
                    Ok(Some(f as u64))
                } else {
                    Err(de::Error::custom(format!("number {f} is not a valid u64")))
                }
            } else {
                Err(de::Error::custom("number is not a valid u64"))
            }
        }
        Some(other) => Err(de::Error::custom(format!(
            "expected u64 or numeric string, got {other}"
        ))),
    }
}

/// Deserialize an `Option<usize>` from a JSON number, a numeric string, or
/// null/missing. Empty strings deserialize to `None`.
pub fn opt_usize_from_string_or_number<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    let parsed = opt_u64_from_string_or_number(deserializer)?;
    parsed
        .map(|v| {
            usize::try_from(v)
                .map_err(|_| de::Error::custom(format!("number {v} out of range for usize")))
        })
        .transpose()
}

struct BoolOrString;

impl<'de> de::Visitor<'de> for BoolOrString {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a bool or a string representing a bool")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<bool, E> {
        Ok(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<bool, E> {
        match v.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" => Ok(true),
            "false" | "0" | "no" | "n" | "" => Ok(false),
            other => Err(E::custom(format!("string {other:?} is not a valid bool"))),
        }
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<bool, E> {
        Ok(v != 0)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<bool, E> {
        Ok(v != 0)
    }
}

/// Deserialize a `bool` from either a JSON bool or a string/number representation.
pub fn bool_from_string_or_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(BoolOrString)
}

/// Deserialize an `Option<bool>` from a JSON bool, a truthy/falsy string, or
/// null/missing. Empty strings deserialize to `None`.
///
/// Same provider quirk as the numeric helpers: models emit `"true"` for a field
/// whose schema says `boolean`, and strict serde rejects the entire tool call.
pub fn opt_bool_from_string_or_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim().to_ascii_lowercase();
            match trimmed.as_str() {
                "" => Ok(None),
                "true" | "1" | "yes" | "y" => Ok(Some(true)),
                "false" | "0" | "no" | "n" => Ok(Some(false)),
                other => Err(de::Error::custom(format!(
                    "string {other:?} is not a valid bool"
                ))),
            }
        }
        Some(serde_json::Value::Number(n)) => {
            if let Some(v) = n.as_u64() {
                Ok(Some(v != 0))
            } else if let Some(v) = n.as_i64() {
                Ok(Some(v != 0))
            } else {
                Err(de::Error::custom("number is not a valid bool"))
            }
        }
        Some(other) => Err(de::Error::custom(format!(
            "expected bool or truthy string, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Demo {
        #[serde(deserialize_with = "u32_from_string_or_number")]
        n: u32,
        #[serde(default, deserialize_with = "opt_u32_from_string_or_number")]
        maybe: Option<u32>,
        #[serde(default, deserialize_with = "bool_from_string_or_bool")]
        flag: bool,
    }

    #[test]
    fn accepts_native_number() {
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 5})).unwrap();
        assert_eq!(d.n, 5);
    }

    #[test]
    fn accepts_string_number() {
        // The #106 case: Claude sends {"compactions": "0"}.
        let d: Demo = serde_json::from_value(serde_json::json!({"n": "0"})).unwrap();
        assert_eq!(d.n, 0);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": "42"})).unwrap();
        assert_eq!(d.n, 42);
    }

    #[test]
    fn rejects_garbage_string() {
        let r: Result<Demo, _> = serde_json::from_value(serde_json::json!({"n": "abc"}));
        assert!(r.is_err());
    }

    #[test]
    fn optional_handles_null_empty_string_and_values() {
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1})).unwrap();
        assert_eq!(d.maybe, None);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "maybe": null})).unwrap();
        assert_eq!(d.maybe, None);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "maybe": ""})).unwrap();
        assert_eq!(d.maybe, None);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "maybe": "7"})).unwrap();
        assert_eq!(d.maybe, Some(7));
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "maybe": 9})).unwrap();
        assert_eq!(d.maybe, Some(9));
    }

    #[test]
    fn bool_accepts_string_and_native() {
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "flag": true})).unwrap();
        assert!(d.flag);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "flag": "true"})).unwrap();
        assert!(d.flag);
        let d: Demo = serde_json::from_value(serde_json::json!({"n": 1, "flag": "false"})).unwrap();
        assert!(!d.flag);
    }
}

#[cfg(test)]
mod coercion_coverage_tests {
    /// Every optional numeric field on a tool input must tolerate a stringified
    /// number.
    ///
    /// Providers emit `"600"` for a field whose schema says `integer`, and
    /// strict serde rejects the *entire* tool call rather than that one field.
    /// The model gets no partial credit, so a whole turn is lost to a
    /// formatting detail. `serde_coerce` was introduced for this exact quirk
    /// (issue #106) but adoption was per-field, so most tools silently kept the
    /// strict behavior; `bg` failed twice in one session on
    /// `max_wait_seconds: "600"`.
    ///
    /// This scans the tool sources rather than testing types one by one,
    /// because the failure mode is precisely a field that *forgot* to opt in.
    #[test]
    fn optional_numeric_tool_fields_use_lenient_deserializers() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tool");
        let mut offenders = Vec::new();

        let entries = std::fs::read_dir(&dir).expect("tool dir");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "serde_coerce.rs" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (idx, line) in lines.iter().enumerate() {
                if line.trim() != "#[serde(default)]" {
                    continue;
                }
                let Some(next) = lines.get(idx + 1) else {
                    continue;
                };
                let next = next.trim();
                // Only optional integer fields; floats and non-numerics are out
                // of scope for the string-number quirk.
                if next.contains(": Option<u64>")
                    || next.contains(": Option<usize>")
                    || next.contains(": Option<u32>")
                {
                    offenders.push(format!("{name}:{}: {next} (numeric)", idx + 2));
                }
                if next.contains(": Option<bool>") {
                    offenders.push(format!("{name}:{}: {next} (bool)", idx + 2));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these optional numeric tool fields use strict serde, so a provider \
             sending \"600\" instead of 600 fails the whole tool call. Add \
             #[serde(deserialize_with = \"super::serde_coerce::opt_*_from_string_or_number\")]  \
             or opt_bool_from_string_or_bool for bool fields:\n{}",
            offenders.join("\n")
        );
    }

    /// End-to-end acceptance: deserialise the actual tool Input structs with
    /// stringified bools, exactly as Fable 5.1 sends them.  We test that
    /// parsing succeeds (or fails for garbage) against the real struct rather
    /// than a Demo, which catches wiring bugs like a missing `deserialize_with`.
    #[test]
    fn cross_tool_bool_coercion_acceptance() {
        // BgInput -- the original Fable 5.1 failure site.
        serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
            "action": "wait",
            "return_on_progress": "false",
            "notify": "true",
            "wake": "0",
            "session_only": "yes",
            "include_output_preview": "1",
        }))
        .expect("BgInput: all stringified bools must parse");

        // BashInput
        serde_json::from_value::<super::super::bash::BashInput>(serde_json::json!({
            "command": "echo ok",
            "run_in_background": "true",
        }))
        .expect("BashInput: stringified bools must parse");

        // BrowserInput
        serde_json::from_value::<super::super::browser::BrowserInput>(serde_json::json!({
            "action": "navigate",
            "url": "https://example.com",
            "new_tab": "true",
            "wait": "false",
            "focus": "1",
            "all_frames": "yes",
        }))
        .expect("BrowserInput: stringified bools must parse");

        // Native bools: regression guard.
        serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
            "action": "wait",
            "return_on_progress": false,
            "notify": true,
        }))
        .expect("BgInput: native bools must still work");

        // Absent bools: default to None, minimal payload.
        serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
            "action": "list"
        }))
        .expect("BgInput: minimal payload with no bools");

        // Null bools.
        serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
            "action": "wait",
            "return_on_progress": null,
        }))
        .expect("BgInput: explicit null bools");

        // Garbage string: must still be rejected.
        assert!(
            serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
                "action": "wait",
                "return_on_progress": "maybe",
            }))
            .is_err(),
            "a non-boolean string must still be rejected"
        );

        // Array/object: must be rejected.
        assert!(
            serde_json::from_value::<super::super::bg::BgInput>(serde_json::json!({
                "action": "wait",
                "return_on_progress": [true],
            }))
            .is_err(),
            "an array value must be rejected"
        );
    }
}

#[cfg(test)]
mod leak_probe_tests {
    /// serde error text quotes the offending value, so any tool that returns a
    /// raw `serde_json::from_value` error hands that value back to the provider
    /// and writes it into the transcript.
    ///
    /// This documents the shape of the problem with a concrete payload. `bash`
    /// is already fixed via `redact_serde_error`; this pins the behavior of the
    /// redactor itself so the remaining tools have a vetted helper to adopt.
    #[test]
    fn redactor_strips_values_serde_would_otherwise_quote() {
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct Input {
            path: String,
        }

        const SECRET: &str = "sk-ant-canary-4417";
        let err = serde_json::from_value::<Input>(serde_json::json!(SECRET))
            .expect_err("a bare string is not this struct");

        // Precondition: without redaction the secret is in the message, which
        // is exactly why this helper exists.
        assert!(
            err.to_string().contains(SECRET),
            "serde is expected to quote the value; if it stopped, this guard is moot"
        );

        let redacted = super::super::redact_serde_error(&err);
        assert!(
            !redacted.contains(SECRET),
            "redaction must remove the quoted value; got: {redacted}"
        );
        // Still useful: the diagnostic prefix survives.
        assert!(
            redacted.contains("invalid type"),
            "redaction must keep the diagnostic; got: {redacted}"
        );
    }
}
