use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::MAX_NAME_BYTES;

pub(super) fn check(bytes: &[u8]) -> Result<(), String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    CheckedValue::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())
}

struct CheckedValue;

impl<'input> Deserialize<'input> for CheckedValue {
    fn deserialize<Decoder: Deserializer<'input>>(
        decoder: Decoder,
    ) -> Result<Self, Decoder::Error> {
        decoder.deserialize_any(CheckedVisitor)
    }
}

struct CheckedVisitor;

impl<'input> Visitor<'input> for CheckedVisitor {
    type Value = CheckedValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("artifact JSON value")
    }

    fn visit_str<Error: de::Error>(self, value: &str) -> Result<Self::Value, Error> {
        if value.len() > MAX_NAME_BYTES {
            return Err(Error::custom("artifact JSON string exceeds 256 bytes"));
        }
        if value.chars().any(char::is_control) {
            return Err(Error::custom(
                "artifact JSON string contains control characters",
            ));
        }
        Ok(CheckedValue)
    }

    fn visit_bool<Error: de::Error>(self, _value: bool) -> Result<Self::Value, Error> {
        Ok(CheckedValue)
    }

    fn visit_i64<Error: de::Error>(self, _value: i64) -> Result<Self::Value, Error> {
        Ok(CheckedValue)
    }

    fn visit_u64<Error: de::Error>(self, _value: u64) -> Result<Self::Value, Error> {
        Ok(CheckedValue)
    }

    fn visit_f64<Error: de::Error>(self, _value: f64) -> Result<Self::Value, Error> {
        Ok(CheckedValue)
    }

    fn visit_unit<Error: de::Error>(self) -> Result<Self::Value, Error> {
        Ok(CheckedValue)
    }

    fn visit_seq<Sequence: SeqAccess<'input>>(
        self,
        mut sequence: Sequence,
    ) -> Result<Self::Value, Sequence::Error> {
        while sequence.next_element::<CheckedValue>()?.is_some() {}
        Ok(CheckedValue)
    }

    fn visit_map<Map: MapAccess<'input>>(self, mut map: Map) -> Result<Self::Value, Map::Error> {
        while map.next_key::<CheckedValue>()?.is_some() {
            map.next_value::<CheckedValue>()?;
        }
        Ok(CheckedValue)
    }
}
