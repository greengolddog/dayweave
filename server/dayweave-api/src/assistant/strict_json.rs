use std::{cell::Cell, collections::BTreeSet, fmt};

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

const MAX_JSON_DEPTH: usize = 32;
const MAX_JSON_VALUES: usize = 16_384;
const MAX_JSON_CONTAINER_ENTRIES: usize = 16_384;
const MAX_JSON_STRING_BYTES: usize = 512 * 1024;

pub(crate) fn parse(input: &[u8]) -> Result<Value, ()> {
    let state = ParseState::default();
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = StrictValueSeed {
        depth: 0,
        state: &state,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    Ok(value)
}

#[derive(Default)]
struct ParseState {
    values: Cell<usize>,
    container_entries: Cell<usize>,
    string_bytes: Cell<usize>,
}

impl ParseState {
    fn claim<E>(counter: &Cell<usize>, amount: usize, limit: usize) -> Result<(), E>
    where
        E: serde::de::Error,
    {
        let next = counter
            .get()
            .checked_add(amount)
            .filter(|next| *next <= limit)
            .ok_or_else(|| E::custom("JSON resource limit exceeded"))?;
        counter.set(next);
        Ok(())
    }
}

#[derive(Clone, Copy)]
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
        ParseState::claim(&self.state.values, 1, MAX_JSON_VALUES)?;
        deserializer.deserialize_any(StrictValueVisitor {
            depth: self.depth,
            state: self.state,
        })
    }
}

#[derive(Clone, Copy)]
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
        formatter.write_str("a bounded JSON value without duplicate keys")
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

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        ParseState::claim(&self.state.string_bytes, value.len(), MAX_JSON_STRING_BYTES)?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        ParseState::claim(&self.state.string_bytes, value.len(), MAX_JSON_STRING_BYTES)?;
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

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let nested = self.nested()?;
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(1_024));
        while let Some(value) = sequence.next_element_seed(nested)? {
            ParseState::claim(&self.state.container_entries, 1, MAX_JSON_CONTAINER_ENTRIES)?;
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
        let mut values = Map::with_capacity(map.size_hint().unwrap_or(0).min(1_024));
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(A::Error::custom("duplicate JSON key"));
            }
            ParseState::claim(&self.state.string_bytes, key.len(), MAX_JSON_STRING_BYTES)?;
            ParseState::claim(&self.state.container_entries, 1, MAX_JSON_CONTAINER_ENTRIES)?;
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
    fn rejects_duplicate_keys_at_any_depth_and_escaped_equivalents() {
        assert!(parse(br#"{"outer":{"value":1,"value":2}}"#).is_err());
        assert!(parse(br#"{"value":1,"\u0076alue":2}"#).is_err());
    }

    #[test]
    fn rejects_trailing_or_excessively_deep_json() {
        assert!(parse(br"{}{}").is_err());
        let deep = format!("{}null{}", "[".repeat(33), "]".repeat(33));
        assert!(parse(deep.as_bytes()).is_err());
    }
}
