//! Serde `Deserializer` implementation for [`TapeRef`].
//!
//! Enabled with the `serde` feature flag.

use crate::dom::{TapeArrayIter, TapeEntryKind, TapeObjectIter, TapeRef};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::fmt;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Deserialization error produced by [`from_taperef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

// ---------------------------------------------------------------------------
// Deserializer for TapeRef<'de, 'de>
//
// Both tape-borrow and source-JSON lifetimes are collapsed to a single `'de`.
// This is the common case when the tape and source both outlive the
// deserialization scope, and it is what `from_taperef` enforces.
// ---------------------------------------------------------------------------

impl<'de> de::Deserializer<'de> for TapeRef<'de, 'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.tape[self.pos].kind() {
            TapeEntryKind::Null => visitor.visit_unit(),
            TapeEntryKind::Bool => visitor.visit_bool(self.tape[self.pos].payload() != 0),
            TapeEntryKind::Number => {
                let s = self.tape[self.pos].as_number().unwrap();
                // Probe integer types before falling back to float.
                if let Ok(v) = s.parse::<u64>() {
                    return visitor.visit_u64(v);
                }
                if let Ok(v) = s.parse::<i64>() {
                    return visitor.visit_i64(v);
                }
                if let Ok(v) = s.parse::<f64>() {
                    return visitor.visit_f64(v);
                }
                visitor.visit_str(s)
            }
            TapeEntryKind::String => {
                // Zero-copy: borrow directly from the source JSON.
                let s: &'de str = self.tape[self.pos].source_string().unwrap();
                visitor.visit_borrowed_str(s)
            }
            TapeEntryKind::EscapedString => {
                visitor.visit_str(self.tape[self.pos].as_string().unwrap())
            }
            TapeEntryKind::StartObject => visitor.visit_map(TapeMapAccess::new(self)),
            TapeEntryKind::StartArray => visitor.visit_seq(TapeSeqAccess::new(self)),
            _ => Err(de::Error::custom("unexpected tape entry at value position")),
        }
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.tape[self.pos].kind() {
            TapeEntryKind::Bool => visitor.visit_bool(self.tape[self.pos].payload() != 0),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_i8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_i64(v)
    }
    fn deserialize_i16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_i64(v)
    }
    fn deserialize_i32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_i64(v)
    }
    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Some(s) = self.tape[self.pos].as_number() {
            visitor.visit_i64(s.parse().map_err(de::Error::custom)?)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_u8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_u64(v)
    }
    fn deserialize_u16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_u64(v)
    }
    fn deserialize_u32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_u64(v)
    }
    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Some(s) = self.tape[self.pos].as_number() {
            visitor.visit_u64(s.parse().map_err(de::Error::custom)?)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_f64(v)
    }
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Some(s) = self.tape[self.pos].as_number() {
            visitor.visit_f64(s.parse().map_err(de::Error::custom)?)
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_char<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.tape[self.pos].kind() {
            TapeEntryKind::String => {
                // Zero-copy borrow from source JSON.
                let s: &'de str = self.tape[self.pos].source_string().unwrap();
                visitor.visit_borrowed_str(s)
            }
            TapeEntryKind::EscapedString => {
                visitor.visit_str(self.tape[self.pos].as_string().unwrap())
            }
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_any(v)
    }
    fn deserialize_byte_buf<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_any(v)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.tape[self.pos].kind() == TapeEntryKind::Null {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.tape[self.pos].kind() == TapeEntryKind::Null {
            visitor.visit_unit()
        } else {
            Err(de::Error::custom("expected null"))
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.tape[self.pos].kind() == TapeEntryKind::StartArray {
            visitor.visit_seq(TapeSeqAccess::new(self))
        } else {
            Err(de::Error::custom("expected JSON array"))
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.tape[self.pos].kind() == TapeEntryKind::StartObject {
            visitor.visit_map(TapeMapAccess::new(self))
        } else {
            Err(de::Error::custom("expected JSON object"))
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.tape[self.pos].kind() {
            // Unit variant: "VariantName"
            TapeEntryKind::String | TapeEntryKind::EscapedString => {
                visitor.visit_enum(UnitVariantAccess(self))
            }
            // Newtype / struct / tuple variant: {"VariantName": <value>}
            TapeEntryKind::StartObject => visitor.visit_enum(TapeEnumAccess::new(self)),
            _ => Err(de::Error::custom(
                "expected string or single-key object for enum",
            )),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_any(v)
    }
}

// ---------------------------------------------------------------------------
// SeqAccess — wraps TapeArrayIter
// ---------------------------------------------------------------------------

struct TapeSeqAccess<'de> {
    iter: TapeArrayIter<'de, 'de>,
}

impl<'de> TapeSeqAccess<'de> {
    fn new(r: TapeRef<'de, 'de>) -> Self {
        Self {
            iter: r.array_iter().expect("expected StartArray entry"),
        }
    }
}

impl<'de> SeqAccess<'de> for TapeSeqAccess<'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        match self.iter.next() {
            None => Ok(None),
            Some(elem) => seed.deserialize(elem).map(Some),
        }
    }
}

// ---------------------------------------------------------------------------
// MapAccess — wraps TapeObjectIter
// ---------------------------------------------------------------------------

struct TapeMapAccess<'de> {
    iter: TapeObjectIter<'de, 'de>,
    /// Stash the value TapeRef between next_key and next_value calls.
    pending_value: Option<TapeRef<'de, 'de>>,
}

impl<'de> TapeMapAccess<'de> {
    fn new(r: TapeRef<'de, 'de>) -> Self {
        Self {
            iter: r.object_iter().expect("expected StartObject entry"),
            pending_value: None,
        }
    }
}

impl<'de> MapAccess<'de> for TapeMapAccess<'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.iter.next() {
            None => Ok(None),
            Some((key, val)) => {
                self.pending_value = Some(val);
                seed.deserialize(KeyDeserializer(key)).map(Some)
            }
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let val = self
            .pending_value
            .take()
            .ok_or_else(|| de::Error::custom("next_value called before next_key"))?;
        seed.deserialize(val)
    }
}

// ---------------------------------------------------------------------------
// KeyDeserializer — borrowed string key from TapeObjectIter
// ---------------------------------------------------------------------------

/// A minimal deserializer for object keys (`&'de str` borrowed from the tape).
struct KeyDeserializer<'de>(&'de str);

impl<'de> de::Deserializer<'de> for KeyDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_borrowed_str(self.0)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct
        map struct enum identifier ignored_any
    }
}

// ---------------------------------------------------------------------------
// Enum support
// ---------------------------------------------------------------------------

/// Unit variant: tape cursor is on a String entry (e.g. `"Foo"`).
struct UnitVariantAccess<'de>(TapeRef<'de, 'de>);

impl<'de> EnumAccess<'de> for UnitVariantAccess<'de> {
    type Error = Error;
    type Variant = UnitOnly;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, UnitOnly), Error> {
        let val = seed.deserialize(self.0)?;
        Ok((val, UnitOnly))
    }
}

struct UnitOnly;

impl<'de> VariantAccess<'de> for UnitOnly {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, _: T) -> Result<T::Value, Error> {
        Err(de::Error::custom("expected unit variant, got newtype"))
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, _: V) -> Result<V::Value, Error> {
        Err(de::Error::custom("expected unit variant, got tuple"))
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Error> {
        Err(de::Error::custom("expected unit variant, got struct"))
    }
}

/// Newtype/struct/tuple variant: `{"VariantName": <payload>}`.
struct TapeEnumAccess<'de> {
    key: &'de str,
    val: TapeRef<'de, 'de>,
}

impl<'de> TapeEnumAccess<'de> {
    fn new(r: TapeRef<'de, 'de>) -> Self {
        let mut iter = r.object_iter().expect("expected StartObject");
        let (key, val) = iter
            .next()
            .expect("enum object must have at least one key-value pair");
        Self { key, val }
    }
}

impl<'de> EnumAccess<'de> for TapeEnumAccess<'de> {
    type Error = Error;
    type Variant = TapeRef<'de, 'de>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, TapeRef<'de, 'de>), Error> {
        let variant = seed.deserialize(KeyDeserializer(self.key))?;
        Ok((variant, self.val))
    }
}

impl<'de> VariantAccess<'de> for TapeRef<'de, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_seq(self, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_map(self, visitor)
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Deserialize a value of type `T` from a [`TapeRef`] cursor.
///
/// `T` must implement [`serde::Deserialize`].  String values with no escape
/// sequences are borrowed zero-copy from the original source JSON for `'de`.
///
/// Pass `tape.root().unwrap()` to deserialize the whole document, or any
/// other `TapeRef` cursor to deserialize a sub-value.
///
/// # Example
///
/// ```rust
/// # #[cfg(feature = "serde")]
/// # {
/// use asmjson::{parse_to_tape, de::from_taperef};
/// use serde::Deserialize;
///
/// #[derive(Deserialize, PartialEq, Debug)]
/// struct Point { x: i64, y: i64 }
///
/// let tape = parse_to_tape(r#"{"x":1,"y":2}"#).unwrap();
/// let root = tape.root().unwrap();
/// let p: Point = from_taperef(root).unwrap();
/// assert_eq!(p, Point { x: 1, y: 2 });
/// # }
/// ```
pub fn from_taperef<'de, T>(r: TapeRef<'de, 'de>) -> Result<T, Error>
where
    T: Deserialize<'de>,
{
    T::deserialize(r)
}
