//! Input constraints shared by deserialization and the advertised JSON schema.

use std::borrow::Cow;
use std::ops::Deref;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

/// Serde structs also accept positional sequences. Tool arguments require
/// named JSON objects at every struct boundary advertised as an object.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub(crate) struct Object<T>(T);

impl<T> Deref for Object<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let value = serde_json::Value::Object(map);
        super::decode(&value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct Number<const MIN: u64, const MAX: u64, const DEFAULT: u64>(u64);

impl<const MIN: u64, const MAX: u64, const DEFAULT: u64> Number<MIN, MAX, DEFAULT> {
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl<const MIN: u64, const MAX: u64, const DEFAULT: u64> Default for Number<MIN, MAX, DEFAULT> {
    fn default() -> Self {
        assert!((MIN..=MAX).contains(&DEFAULT));
        Self(DEFAULT)
    }
}

impl<'de, const MIN: u64, const MAX: u64, const DEFAULT: u64> Deserialize<'de>
    for Number<MIN, MAX, DEFAULT>
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if !(MIN..=MAX).contains(&value) {
            return Err(serde::de::Error::custom(format!(
                "must be between {MIN} and {MAX}"
            )));
        }
        Ok(Self(value))
    }
}

impl<const MIN: u64, const MAX: u64, const DEFAULT: u64> JsonSchema for Number<MIN, MAX, DEFAULT> {
    fn schema_name() -> Cow<'static, str> {
        format!("Number_{MIN}_{MAX}_{DEFAULT}").into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({"type": "integer", "minimum": MIN, "maximum": MAX})
    }
}

/// Nonblank text. Limits count Unicode characters, as JSON Schema does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct Text<const MAX: usize = { usize::MAX }>(String);

impl<const MAX: usize> Deref for Text<MAX> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> Text<MAX> {
    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Text<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(serde::de::Error::custom("must be nonblank text"));
        }
        if value.chars().count() > MAX {
            return Err(serde::de::Error::custom(format!(
                "must be at most {MAX} characters"
            )));
        }
        Ok(Self(value))
    }
}

impl<const MAX: usize> JsonSchema for Text<MAX> {
    fn schema_name() -> Cow<'static, str> {
        format!("Text_{MAX}").into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let mut schema =
            schemars::json_schema!({"type": "string", "minLength": 1, "pattern": "\\S"});
        if MAX != usize::MAX {
            schema.insert("maxLength".into(), MAX.into());
        }
        schema
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct NonEmpty<T>(Vec<T>);

impl<T> Deref for NonEmpty<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NonEmpty<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<T>::deserialize(deserializer)?;
        if values.is_empty() {
            return Err(serde::de::Error::custom("must contain at least one entry"));
        }
        Ok(Self(values))
    }
}

impl<T: JsonSchema> JsonSchema for NonEmpty<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("NonEmpty_{}", T::schema_name()).into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = Vec::<T>::json_schema(generator);
        schema.insert("minItems".into(), 1.into());
        schema
    }
}
