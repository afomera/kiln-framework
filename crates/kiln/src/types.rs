use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::{Row, ValueRef, sqlite::SqliteRow};

/// A typed primary key. `Id<Post>` and `Id<User>` are different types, so a foreign key
/// can't be passed where another table's key is expected.
pub struct Id<T> {
    raw: i64,
    _record: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    pub const fn new(raw: i64) -> Self {
        Self { raw, _record: PhantomData }
    }

    pub const fn get(self) -> i64 {
        self.raw
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<T>();
        write!(f, "Id<{}>({})", name.rsplit("::").next().unwrap_or(name), self.raw)
    }
}

impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.raw.fmt(f)
    }
}

impl<T> FromStr for Id<T> {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Id::new)
    }
}

impl<T> Serialize for Id<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.raw)
    }
}

impl<'de, T> Deserialize<'de> for Id<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        i64::deserialize(deserializer).map(Id::new)
    }
}

/// A UTC instant. Stored as integer microseconds so SQL ordering is exact; rendered as RFC 3339.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    pub fn from_jiff(ts: jiff::Timestamp) -> Self {
        Self(ts)
    }

    pub fn as_jiff(&self) -> jiff::Timestamp {
        self.0
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({})", self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Timestamp {
    type Err = jiff::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// A value bound into a SQL statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

/// Converts a Rust value into a SQL bind value.
pub trait ToValue {
    fn to_value(&self) -> Value;
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToValue for i64 {
    fn to_value(&self) -> Value {
        Value::Integer(*self)
    }
}

impl ToValue for i32 {
    fn to_value(&self) -> Value {
        Value::Integer(i64::from(*self))
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Integer(i64::from(*self))
    }
}

impl ToValue for f64 {
    fn to_value(&self) -> Value {
        Value::Real(*self)
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToValue::to_value)
    }
}

impl<T> ToValue for Id<T> {
    fn to_value(&self) -> Value {
        Value::Integer(self.raw)
    }
}

impl ToValue for Timestamp {
    fn to_value(&self) -> Value {
        Value::Integer(self.0.as_microsecond())
    }
}

/// Reads a column out of a row.
pub trait FromColumn: Sized {
    fn from_column(row: &SqliteRow, name: &str) -> Result<Self, sqlx::Error>;
}

macro_rules! native_from_column {
    ($($t:ty),*) => {$(
        impl FromColumn for $t {
            fn from_column(row: &SqliteRow, name: &str) -> Result<Self, sqlx::Error> {
                row.try_get(name)
            }
        }
    )*};
}

native_from_column!(String, i64, i32, bool, f64);

impl<T: FromColumn> FromColumn for Option<T> {
    fn from_column(row: &SqliteRow, name: &str) -> Result<Self, sqlx::Error> {
        if row.try_get_raw(name)?.is_null() { Ok(None) } else { T::from_column(row, name).map(Some) }
    }
}

impl<T> FromColumn for Id<T> {
    fn from_column(row: &SqliteRow, name: &str) -> Result<Self, sqlx::Error> {
        row.try_get::<i64, _>(name).map(Id::new)
    }
}

impl FromColumn for Timestamp {
    fn from_column(row: &SqliteRow, name: &str) -> Result<Self, sqlx::Error> {
        let micros: i64 = row.try_get(name)?;
        jiff::Timestamp::from_microsecond(micros)
            .map(Timestamp)
            .map_err(|e| sqlx::Error::ColumnDecode { index: name.to_owned(), source: Box::new(e) })
    }
}

/// Maps a Rust type to its SQL column type.
pub trait SqlType {
    const SQL: &'static str;
    const NULLABLE: bool = false;
    /// Allowed values, for enum-like types. Rendered as a CHECK constraint.
    const VARIANTS: &'static [&'static str] = &[];
}

impl SqlType for String {
    const SQL: &'static str = "TEXT";
}

impl SqlType for i64 {
    const SQL: &'static str = "INTEGER";
}

impl SqlType for i32 {
    const SQL: &'static str = "INTEGER";
}

impl SqlType for bool {
    const SQL: &'static str = "INTEGER";
}

impl SqlType for f64 {
    const SQL: &'static str = "REAL";
}

impl<T> SqlType for Id<T> {
    const SQL: &'static str = "INTEGER";
}

impl SqlType for Timestamp {
    const SQL: &'static str = "INTEGER";
}

impl<T: SqlType> SqlType for Option<T> {
    const SQL: &'static str = T::SQL;
    const NULLABLE: bool = true;
    const VARIANTS: &'static [&'static str] = T::VARIANTS;
}
