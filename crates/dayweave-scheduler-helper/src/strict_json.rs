use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_VALUES: usize = 500_000;
const MAX_JSON_CONTAINER_ENTRIES: usize = 500_000;
const MAX_JSON_STRING_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrictJsonError {
    Invalid,
    DuplicateKey,
    DepthExceeded,
    ResourceLimit,
}

pub(crate) fn parse(input: &[u8]) -> Result<Value, StrictJsonError> {
    let state = ParseState::default();
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = StrictValueSeed {
        depth: 0,
        state: &state,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| state.failure.get().unwrap_or(StrictJsonError::Invalid))?;
    deserializer
        .end()
        .map_err(|_| state.failure.get().unwrap_or(StrictJsonError::Invalid))?;
    Ok(value)
}

#[derive(Debug, Default)]
struct ParseState {
    failure: Cell<Option<StrictJsonError>>,
    values: Cell<usize>,
    container_entries: Cell<usize>,
    string_bytes: Cell<usize>,
}

impl ParseState {
    fn claim_value<E>(&self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.claim(&self.values, 1, MAX_JSON_VALUES)
    }

    fn claim_container_entry<E>(&self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.claim(&self.container_entries, 1, MAX_JSON_CONTAINER_ENTRIES)
    }

    fn claim_string<E>(&self, bytes: usize) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.claim(&self.string_bytes, bytes, MAX_JSON_STRING_BYTES)
    }

    fn claim<E>(&self, counter: &Cell<usize>, amount: usize, limit: usize) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        let Some(next) = counter.get().checked_add(amount) else {
            return self.resource_error();
        };
        if next > limit {
            return self.resource_error();
        }
        counter.set(next);
        Ok(())
    }

    fn resource_error<E>(&self) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        self.failure.set(Some(StrictJsonError::ResourceLimit));
        Err(E::custom("JSON resource limit exceeded"))
    }
}

#[derive(Debug, Clone, Copy)]
struct StrictValueSeed<'a> {
    depth: usize,
    state: &'a ParseState,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        self.state.claim_value()?;
        deserializer.deserialize_any(StrictValueVisitor {
            depth: self.depth,
            state: self.state,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct StrictValueVisitor<'a> {
    depth: usize,
    state: &'a ParseState,
}

impl<'a> StrictValueVisitor<'a> {
    fn nested<E>(self) -> Result<StrictValueSeed<'a>, E>
    where
        E: serde::de::Error,
    {
        if self.depth >= MAX_JSON_DEPTH {
            self.state.failure.set(Some(StrictJsonError::DepthExceeded));
            return Err(E::custom("JSON nesting limit exceeded"));
        }
        Ok(StrictValueSeed {
            depth: self.depth + 1,
            state: self.state,
        })
    }
}

impl<'de> Visitor<'de> for StrictValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_char<E>(self, value: char) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.state.claim_string(value.len_utf8())?;
        Ok(Value::String(value.to_string()))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.state.claim_string(value.len())?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.state.claim_string(value.len())?;
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValueSeed {
            depth: self.depth,
            state: self.state,
        }
        .deserialize(deserializer)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValueSeed {
            depth: self.depth,
            state: self.state,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let nested = self.nested()?;
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(4_096));
        while let Some(value) = sequence.next_element_seed(nested)? {
            self.state.claim_container_entry()?;
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let nested = self.nested()?;
        let mut keys = BTreeSet::new();
        let mut values = Map::with_capacity(map.size_hint().unwrap_or(0).min(4_096));
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                self.state.failure.set(Some(StrictJsonError::DuplicateKey));
                return Err(A::Error::custom("duplicate JSON key"));
            }
            self.state.claim_string(key.len())?;
            self.state.claim_container_entry()?;
            let value = map.next_value_seed(nested)?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_keys_at_any_depth() {
        assert_eq!(
            parse(br#"{"outer":{"value":1,"value":2}}"#),
            Err(StrictJsonError::DuplicateKey)
        );
        assert_eq!(
            parse(br#"{"value":1,"\u0076alue":2}"#),
            Err(StrictJsonError::DuplicateKey)
        );
    }

    #[test]
    fn enforces_the_container_depth_limit() {
        let accepted = format!("{}null{}", "[".repeat(64), "]".repeat(64));
        assert!(parse(accepted.as_bytes()).is_ok());

        let rejected = format!("{}null{}", "[".repeat(65), "]".repeat(65));
        assert_eq!(
            parse(rejected.as_bytes()),
            Err(StrictJsonError::DepthExceeded)
        );
    }

    #[test]
    fn rejects_trailing_json() {
        assert_eq!(parse(br"{}{}"), Err(StrictJsonError::Invalid));
    }

    #[test]
    fn rejects_wide_shallow_documents_before_building_an_unbounded_ast() {
        let mut wide = String::with_capacity(MAX_JSON_VALUES.saturating_mul(2));
        wide.push('[');
        for index in 0..=MAX_JSON_VALUES {
            if index > 0 {
                wide.push(',');
            }
            wide.push('0');
        }
        wide.push(']');
        assert_eq!(parse(wide.as_bytes()), Err(StrictJsonError::ResourceLimit));
    }
}
