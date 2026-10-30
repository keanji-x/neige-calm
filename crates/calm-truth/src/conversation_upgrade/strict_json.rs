//! Standard serde JSON parser with ordered member occurrences, never a Value/Map.
use super::facts::{JsonAddress, JsonKind, JsonStep};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::{collections::HashMap, fmt};

pub(super) struct JsonNodes {
    pub nodes: Vec<(JsonAddress, JsonKind)>,
    pub duplicates: Vec<(JsonAddress, JsonAddress)>,
    incomplete: bool,
    number_out_of_range: bool,
}
pub(super) enum JsonFailure {
    Syntax,
    Incomplete,
}
pub(super) fn parse(text: &str) -> Result<JsonNodes, JsonFailure> {
    let mut output = JsonNodes {
        nodes: Vec::new(),
        duplicates: Vec::new(),
        incomplete: false,
        number_out_of_range: false,
    };
    let mut de = serde_json::Deserializer::from_str(text);
    let result = Seed {
        output: &mut output,
        path: JsonAddress::default(),
    }
    .deserialize(&mut de)
    .and_then(|()| de.end());
    match result {
        Ok(()) if output.number_out_of_range => Err(JsonFailure::Incomplete),
        Ok(()) => Ok(output),
        Err(_) if output.incomplete => Err(JsonFailure::Incomplete),
        Err(_) => Err(JsonFailure::Syntax),
    }
}
struct Seed<'a> {
    output: &'a mut JsonNodes,
    path: JsonAddress,
}
impl Seed<'_> {
    fn record(&mut self, kind: JsonKind) {
        self.output.nodes.push((self.path.clone(), kind));
    }
    fn child(&mut self, step: JsonStep) -> Seed<'_> {
        let mut path = self.path.clone();
        path.0.push(step);
        Seed {
            output: self.output,
            path,
        }
    }
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        // Explicit coverage limits stay below serde's recursion limit. No
        // partially traversed document can become a CompleteSourceFacts.
        if self.path.0.len() >= 64 || self.output.nodes.len() >= 100_000 {
            self.output.incomplete = true;
            return Err(serde::de::Error::custom("source traversal limit"));
        }
        // RawValue only frames an unchanged node, never proves string validity.
        // The normal Visitor below decodes every string and key, including after
        // an overflowing number. No Value/Map can erase duplicate occurrences.
        let raw = <&serde_json::value::RawValue>::deserialize(de)?;
        let mut parser = serde_json::Deserializer::from_str(raw.get());
        let Seed { output, path } = self;
        match parser.deserialize_any(Seed { output, path }) {
            Ok(()) => parser.end().map_err(serde::de::Error::custom),
            // No public typed overflow code exists. Children handle their own
            // overflow, so this error is confined to a numeric leaf. RawValue's
            // standard number grammar framed its full original token; strings
            // and containers must still succeed through the normal Visitor.
            Err(e) if e.to_string().starts_with("number out of range") => {
                output.number_out_of_range = true;
                Ok(())
            }
            Err(e) => Err(serde::de::Error::custom(e)),
        }
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON source node")
    }
    fn visit_unit<E: serde::de::Error>(mut self) -> Result<(), E> {
        self.record(JsonKind::Null);
        Ok(())
    }
    fn visit_bool<E: serde::de::Error>(mut self, _: bool) -> Result<(), E> {
        self.record(JsonKind::Bool);
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(mut self, _: i64) -> Result<(), E> {
        self.record(JsonKind::Number);
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(mut self, _: u64) -> Result<(), E> {
        self.record(JsonKind::Number);
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(mut self, _: f64) -> Result<(), E> {
        self.record(JsonKind::Number);
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(mut self, _: &str) -> Result<(), E> {
        self.record(JsonKind::String);
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut a: A) -> Result<(), A::Error> {
        self.record(JsonKind::Array);
        let mut index = 0;
        while a
            .next_element_seed(self.child(JsonStep::Index(index)))?
            .is_some()
        {
            index += 1;
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut a: A) -> Result<(), A::Error> {
        self.record(JsonKind::Object);
        let mut members = HashMap::<String, (usize, usize)>::new();
        let mut ordinal = 0;
        while let Some(name) = a.next_key::<String>()? {
            let (first, occurrence) = members.entry(name.clone()).or_insert((ordinal, 0));
            let step = JsonStep::Member {
                name: name.clone(),
                ordinal,
                occurrence: *occurrence,
            };
            if *occurrence > 0 {
                let mut first_path = self.path.clone();
                first_path.0.push(JsonStep::Member {
                    name,
                    ordinal: *first,
                    occurrence: 0,
                });
                let mut repeated = self.path.clone();
                repeated.0.push(step.clone());
                self.output.duplicates.push((first_path, repeated));
            }
            *occurrence += 1;
            a.next_value_seed(self.child(step))?;
            ordinal += 1;
        }
        Ok(())
    }
}
