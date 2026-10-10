use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor},
    ser::SerializeMap,
};
use serde_json::value::RawValue;
use std::fmt;

/// Preserve member order and unrelated JSON bytes only at the settings boundary.
#[derive(Clone, Default)]
pub(super) struct OrderedObject(Vec<(String, Box<RawValue>)>);
impl OrderedObject {
    pub fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_ref())
    }
    pub fn set(&mut self, key: &str, value: &impl Serialize) -> Result<(), String> {
        let value = serde_json::value::to_raw_value(value).map_err(|error| error.to_string())?;
        if let Some((_, entry)) = self.0.iter_mut().find(|(name, _)| name == key) {
            *entry = value;
        } else {
            self.0.push((key.to_owned(), value));
        }
        Ok(())
    }
}
impl Serialize for OrderedObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = OrderedObject;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if entries.iter().any(|(name, _)| name == &key) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    entries.push((key, value));
                }
                Ok(OrderedObject(entries))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}
