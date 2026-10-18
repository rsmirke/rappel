//! The JSON of a legacy collection (`col.models`, `col.decks`) and of a
//! legacy media list, read with bounds (ADR 0061): every object and list is
//! counted as it is read and refused past its limit, every string is kept
//! only up to its limit (longer ones are marked, not copied), and anything
//! not asked for is skipped without being kept. So the memory of a read is
//! bounded by the limits, not by what the JSON declares.

use std::fmt;

use serde::Deserializer as _;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// A string as it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Text {
    /// Within its limit.
    Kept(String),
    /// Longer than its limit (not kept).
    TooLong,
    /// Not a string (or missing).
    Other,
}

impl Text {
    pub(super) fn kept(self) -> Option<String> {
        match self {
            Self::Kept(s) => Some(s),
            Self::TooLong | Self::Other => None,
        }
    }
}

/// Reads any value; a string is kept up to `max` bytes.
#[derive(Clone, Copy)]
struct TextSeed(usize);

impl<'de> DeserializeSeed<'de> for TextSeed {
    type Value = Text;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Text, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TextSeed {
    type Value = Text;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Text, E> {
        Ok(if v.len() <= self.0 {
            Text::Kept(v.to_owned())
        } else {
            Text::TooLong
        })
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Text, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Text::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Text, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Text::Other)
    }
}

/// Reads any value; an integer is kept.
#[derive(Clone, Copy)]
struct IntSeed;

impl<'de> DeserializeSeed<'de> for IntSeed {
    type Value = Option<i64>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Option<i64>, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for IntSeed {
    type Value = Option<i64>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<i64>, E> {
        Ok(Some(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Option<i64>, E> {
        Ok(i64::try_from(v).ok())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<i64>, E> {
        Ok(None)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Option<i64>, E> {
        Ok(None)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Option<i64>, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Option<i64>, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Option<i64>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<i64>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

/// The limits of a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Bounds {
    /// Entries of the top-level object.
    pub(super) entries: usize,
    /// Items of one list (a note type's fields or templates).
    pub(super) items: usize,
    /// Items of all lists together.
    pub(super) items_total: usize,
    /// A name, in bytes.
    pub(super) name: usize,
    /// A template, in bytes.
    pub(super) template: usize,
}

/// One field of a legacy note type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawField {
    pub(super) name: Text,
    pub(super) ord: Option<i64>,
}

/// One template of a legacy note type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawTemplate {
    pub(super) name: Text,
    pub(super) ord: Option<i64>,
    pub(super) qfmt: Text,
    pub(super) afmt: Text,
}

/// A legacy note type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RawModel {
    pub(super) name: Option<Text>,
    pub(super) kind: Option<i64>,
    pub(super) fields: Vec<RawField>,
    pub(super) templates: Vec<RawTemplate>,
}

/// Why a read failed: the message, and whether a count limit was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refused(pub(super) String, pub(super) bool);

fn too_many<E: de::Error>(what: &str) -> E {
    E::custom(format!("more {what} than allowed"))
}

/// Visits the top-level object entry by entry.
struct ObjectVisitor<'a, S: Each> {
    entries: usize,
    seed: S,
    out: &'a mut Vec<(Text, <S as Each>::Out)>,
}

/// What reads one value of the top-level object.
trait Each: Copy {
    type Out;
    fn read<'de, A: MapAccess<'de>>(&self, map: &mut A) -> Result<Self::Out, A::Error>;
}

impl<'de, S: Each> Visitor<'de> for ObjectVisitor<'_, S> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key_seed(TextSeed(64))? {
            if self.out.len() == self.entries {
                return Err(too_many("entries"));
            }
            let value = self.seed.read(&mut map)?;
            self.out.push((key, value));
        }
        Ok(())
    }
}

fn object<S: Each>(json: &[u8], entries: usize, seed: S) -> Result<Vec<(Text, S::Out)>, Refused> {
    let mut out = Vec::new();
    let mut de = serde_json::Deserializer::from_slice(json);
    de.deserialize_map(ObjectVisitor {
        entries,
        seed,
        out: &mut out,
    })
    .and_then(|()| de.end())
    .map_err(|e| {
        let message = e.to_string();
        let too_many = message.starts_with("more ");
        Refused(message, too_many)
    })?;
    Ok(out)
}

/// A media list value: the file's name.
#[derive(Clone, Copy)]
struct MediaName(usize);

impl Each for MediaName {
    type Out = Text;

    fn read<'de, A: MapAccess<'de>>(&self, map: &mut A) -> Result<Text, A::Error> {
        map.next_value_seed(TextSeed(self.0))
    }
}

/// A legacy media list, `{"0": "name.png", …}`: (key, name) pairs.
pub(super) fn media_list(
    json: &[u8],
    entries: usize,
    name: usize,
) -> Result<Vec<(Text, Text)>, Refused> {
    object(json, entries, MediaName(name))
}

/// A deck value: its `name`.
#[derive(Clone, Copy)]
struct DeckSeed(usize);

impl Each for DeckSeed {
    type Out = Text;

    fn read<'de, A: MapAccess<'de>>(&self, map: &mut A) -> Result<Text, A::Error> {
        map.next_value_seed(*self)
    }
}

impl<'de> DeserializeSeed<'de> for DeckSeed {
    type Value = Text;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Text, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for DeckSeed {
    type Value = Text;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a deck")
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Text, E> {
        Ok(Text::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Text, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Text::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Text, A::Error> {
        let mut name = Text::Other;
        while let Some(key) = map.next_key_seed(TextSeed(16))? {
            if key == Text::Kept("name".to_owned()) {
                name = map.next_value_seed(TextSeed(self.0))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(name)
    }
}

/// A legacy `col.decks`: (id, name) pairs.
pub(super) fn decks(
    json: &[u8],
    entries: usize,
    name: usize,
) -> Result<Vec<(Text, Text)>, Refused> {
    object(json, entries, DeckSeed(name))
}

/// A note type value, with a shared count of list items.
#[derive(Clone, Copy)]
struct ModelSeed<'c> {
    bounds: Bounds,
    total: &'c std::cell::Cell<usize>,
}

impl Each for ModelSeed<'_> {
    type Out = RawModel;

    fn read<'de, A: MapAccess<'de>>(&self, map: &mut A) -> Result<RawModel, A::Error> {
        map.next_value_seed(*self)
    }
}

impl<'de> DeserializeSeed<'de> for ModelSeed<'_> {
    type Value = RawModel;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<RawModel, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ModelSeed<'_> {
    type Value = RawModel;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a note type")
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_unit<E: de::Error>(self) -> Result<RawModel, E> {
        Ok(RawModel::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<RawModel, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(RawModel::default())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawModel, A::Error> {
        let mut model = RawModel::default();
        while let Some(key) = map.next_key_seed(TextSeed(16))? {
            match key {
                Text::Kept(k) if k == "name" => {
                    model.name = Some(map.next_value_seed(TextSeed(self.bounds.name))?);
                }
                Text::Kept(k) if k == "type" => model.kind = map.next_value_seed(IntSeed)?,
                Text::Kept(k) if k == "flds" => {
                    model.fields = map
                        .next_value_seed(ListSeed {
                            model: self,
                            template: false,
                        })?
                        .0;
                }
                Text::Kept(k) if k == "tmpls" => {
                    model.templates = map
                        .next_value_seed(ListSeed {
                            model: self,
                            template: true,
                        })?
                        .1;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(model)
    }
}

/// A list of fields or templates.
#[derive(Clone, Copy)]
struct ListSeed<'c> {
    model: ModelSeed<'c>,
    template: bool,
}

impl<'de> DeserializeSeed<'de> for ListSeed<'_> {
    type Value = (Vec<RawField>, Vec<RawTemplate>);

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ListSeed<'_> {
    type Value = (Vec<RawField>, Vec<RawTemplate>);

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let (mut fields, mut templates) = (Vec::new(), Vec::new());
        let bounds = self.model.bounds;
        while let Some(item) = seq.next_element_seed(ItemSeed(bounds, self.template))? {
            let n = fields.len() + templates.len();
            let total = self.model.total.get() + 1;
            if n == bounds.items || total > bounds.items_total {
                return Err(too_many("fields or templates"));
            }
            self.model.total.set(total);
            if self.template {
                templates.push(item.1);
            } else {
                fields.push(item.0);
            }
        }
        Ok((fields, templates))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok((Vec::new(), Vec::new()))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok((Vec::new(), Vec::new()))
    }
}

/// One field, or with `.1` one template: only the one asked for is kept.
#[derive(Clone, Copy)]
struct ItemSeed(Bounds, bool);

impl<'de> DeserializeSeed<'de> for ItemSeed {
    type Value = (RawField, RawTemplate);

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ItemSeed {
    type Value = (RawField, RawTemplate);

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a field or template")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut t = RawTemplate {
            name: Text::Other,
            ord: None,
            qfmt: Text::Other,
            afmt: Text::Other,
        };
        while let Some(key) = map.next_key_seed(TextSeed(16))? {
            match key {
                Text::Kept(k) if k == "name" => {
                    t.name = map.next_value_seed(TextSeed(self.0.name))?;
                }
                Text::Kept(k) if k == "ord" => t.ord = map.next_value_seed(IntSeed)?,
                Text::Kept(k) if k == "qfmt" => {
                    t.qfmt = map.next_value_seed(TextSeed(self.0.template))?;
                }
                Text::Kept(k) if k == "afmt" => {
                    t.afmt = map.next_value_seed(TextSeed(self.0.template))?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        // Each name is kept once: in the field, or in the template.
        if self.1 {
            Ok((Self::other().0, t))
        } else {
            let field = RawField {
                name: t.name,
                ord: t.ord,
            };
            Ok((field, Self::other().1))
        }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Self::other())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(Self::other())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(Self::other())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(Self::other())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(Self::other())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(Self::other())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Self::other())
    }
}

impl ItemSeed {
    fn other() -> (RawField, RawTemplate) {
        (
            RawField {
                name: Text::Other,
                ord: None,
            },
            RawTemplate {
                name: Text::Other,
                ord: None,
                qfmt: Text::Other,
                afmt: Text::Other,
            },
        )
    }
}

/// A legacy `col.models`: (id, note type) pairs.
pub(super) fn models(json: &[u8], bounds: Bounds) -> Result<Vec<(Text, RawModel)>, Refused> {
    let total = std::cell::Cell::new(0);
    object(
        json,
        bounds.entries,
        ModelSeed {
            bounds,
            total: &total,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Bounds = Bounds {
        entries: 3,
        items: 4,
        items_total: 6,
        name: 8,
        template: 16,
    };

    #[test]
    fn note_types_are_read_within_their_bounds() {
        let json = br#"{"1": {"name": "Basic", "type": 0, "x": [1, {"y": 2}],
            "flds": [{"name": "Front", "ord": 0}, {"name": "Back", "ord": 1, "sticky": false}],
            "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "a very long template here"}]}}"#;
        let models = models(json, BOUNDS).unwrap();
        assert_eq!(models.len(), 1);
        let (id, m) = &models[0];
        assert_eq!(*id, Text::Kept("1".to_owned()));
        assert_eq!(m.name, Some(Text::Kept("Basic".to_owned())));
        assert_eq!(m.kind, Some(0));
        assert_eq!(m.fields.len(), 2);
        assert_eq!(m.fields[1].name, Text::Kept("Back".to_owned()));
        assert_eq!(m.templates[0].qfmt, Text::Kept("{{Front}}".to_owned()));
        assert_eq!(m.templates[0].afmt, Text::TooLong);
    }

    #[test]
    fn too_many_entries_or_items_are_refused() {
        assert!(models(br#"{"1":{},"2":{},"3":{},"4":{}}"#, BOUNDS).is_err());
        assert!(models(br#"{"1":{"flds":[{},{},{},{},{}]}}"#, BOUNDS).is_err());
        // Six items in all, over several note types.
        assert!(
            models(
                br#"{"1":{"flds":[{},{},{}]},"2":{"tmpls":[{},{},{}]},"3":{"flds":[{}]}}"#,
                BOUNDS
            )
            .is_err()
        );
        assert!(
            models(
                br#"{"1":{"flds":[{},{},{}]},"2":{"tmpls":[{},{},{}]}}"#,
                BOUNDS
            )
            .is_ok()
        );
        assert!(decks(br#"{"1":{},"2":{},"3":{},"4":{}}"#, 3, 8).is_err());
        assert!(media_list(br#"{"0":"a","1":"b","2":"c","3":"d"}"#, 3, 8).is_err());
    }

    #[test]
    fn other_values_are_skipped_and_names_bounded() {
        let decks = decks(
            br#"{"1": {"name": "a very long name"}, "2": 5, "3": {"name": [1]}}"#,
            3,
            8,
        )
        .unwrap();
        assert_eq!(
            decks,
            [
                (Text::Kept("1".into()), Text::TooLong),
                (Text::Kept("2".into()), Text::Other),
                (Text::Kept("3".into()), Text::Other),
            ]
        );
        assert!(models(b"[1, 2]", BOUNDS).is_err());
        assert!(models(b"{\"1\": {}} trailing", BOUNDS).is_err());
    }
}
