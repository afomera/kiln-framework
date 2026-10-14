//! Relations and preloading.
//!
//! A relation is never a lazily loaded field. `Query::preload` changes the query's type, and
//! the accessor (`post.author()`) only resolves when that relation is in the
//! `Loaded<T, (..)>` tuple. Forgetting to preload is a compile error, so N+1 queries can't
//! be written.

use std::{collections::HashMap, fmt, future::Future, marker::PhantomData, ops::Deref, pin::Pin};

use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::{Column, Db, Error, Id, Query, Record, Result};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A relation from `Owner` to other records. `#[derive(Record)]` generates one marker type
/// per `belongs_to`/`has_many` entry, e.g. `post::author`.
pub trait Relation<Owner: Record>: Send + Sync + 'static {
    /// What each owner gets: one row for `belongs_to`, a `Vec` for `has_many`.
    type Out: Clone + Send + Sync + Serialize + 'static;
    const NAME: &'static str;

    /// Loads the relation for every owner in one query. The result is aligned with `owners`.
    fn load<'a>(owners: &'a [Owner], db: &'a Db) -> BoxFuture<'a, Result<Vec<Self::Out>>>;
}

/// A tuple of relations preloaded on `T`. `()` means none.
pub trait RelSet<T: Record>: Send + Sync + 'static {
    type Data: Clone + Send + Sync + 'static;
    /// `T` itself when nothing is preloaded, `Loaded<T, Self>` otherwise.
    type Row: Clone + Send + Sync + Serialize + 'static;

    fn load<'a>(owners: &'a [T], db: &'a Db) -> BoxFuture<'a, Result<Vec<Self::Data>>>;
    fn assemble(record: T, data: Self::Data) -> Self::Row;
    fn record(row: &Self::Row) -> &T;
    fn serialize_data<M: SerializeMap>(data: &Self::Data, map: &mut M) -> std::result::Result<(), M::Error>;
}

impl<T: Record> RelSet<T> for () {
    type Data = ();
    type Row = T;

    fn load<'a>(owners: &'a [T], _db: &'a Db) -> BoxFuture<'a, Result<Vec<()>>> {
        Box::pin(async move { Ok(vec![(); owners.len()]) })
    }

    fn assemble(record: T, _data: ()) -> T {
        record
    }

    fn record(row: &T) -> &T {
        row
    }

    fn serialize_data<M: SerializeMap>(_: &(), _: &mut M) -> std::result::Result<(), M::Error> {
        Ok(())
    }
}

macro_rules! rel_set_tuple {
    ($($A:ident $a:ident $n:tt),+) => {
        impl<T: Record, $($A: Relation<T>),+> RelSet<T> for ($($A,)+) {
            type Data = ($(<$A as Relation<T>>::Out,)+);
            type Row = Loaded<T, Self>;

            fn load<'a>(owners: &'a [T], db: &'a Db) -> BoxFuture<'a, Result<Vec<Self::Data>>> {
                Box::pin(async move {
                    $(
                        let $a = <$A as Relation<T>>::load(owners, db).await?;
                        if $a.len() != owners.len() {
                            return Err(Error::Integrity(format!(
                                "relation `{}` returned {} rows for {} owners",
                                <$A as Relation<T>>::NAME, $a.len(), owners.len()
                            )));
                        }
                        let mut $a = $a.into_iter();
                    )+
                    let mut out = Vec::with_capacity(owners.len());
                    for _ in 0..owners.len() {
                        out.push(($($a.next().expect("length checked above"),)+));
                    }
                    Ok(out)
                })
            }

            fn assemble(record: T, data: Self::Data) -> Self::Row {
                Loaded { record, data }
            }

            fn record(row: &Self::Row) -> &T {
                &row.record
            }

            fn serialize_data<M: SerializeMap>(data: &Self::Data, map: &mut M) -> std::result::Result<(), M::Error> {
                $( map.serialize_entry(<$A as Relation<T>>::NAME, &data.$n)?; )+
                Ok(())
            }
        }
    };
}

rel_set_tuple!(A a 0);
rel_set_tuple!(A a 0, B b 1);
rel_set_tuple!(A a 0, B b 1, C c 2);
rel_set_tuple!(A a 0, B b 1, C c 2, D d 3);
rel_set_tuple!(A a 0, B b 1, C c 2, D d 3, E e 4);
rel_set_tuple!(A a 0, B b 1, C c 2, D d 3, E e 4, F f 5);

/// Type-level append: `(A,)` + `B` = `(A, B)`. Used by `preload`.
pub trait Append<X> {
    type Output;
}

impl<X> Append<X> for () {
    type Output = (X,);
}

macro_rules! append_tuple {
    ($($A:ident),+) => {
        impl<$($A,)+ X> Append<X> for ($($A,)+) {
            type Output = ($($A,)+ X);
        }
    };
}

append_tuple!(A);
append_tuple!(A, B);
append_tuple!(A, B, C);
append_tuple!(A, B, C, D);
append_tuple!(A, B, C, D, E);

/// Positions in a relation tuple. They let the compiler find a relation by type without
/// overlapping impls. Never written by hand.
pub mod idx {
    pub struct I0;
    pub struct I1;
    pub struct I2;
    pub struct I3;
    pub struct I4;
    pub struct I5;
}

/// `Self` (a relation tuple) contains relation `X` at position `I`.
#[diagnostic::on_unimplemented(
    message = "relation `{X}` was not preloaded",
    label = "the preloaded relations are `{Self}`",
    note = "add `.preload(...)` for this relation to the query, and include it in the `Loaded<..>` type"
)]
pub trait HasRel<T: Record, X: Relation<T>, I>: RelSet<T> {
    fn get(data: &Self::Data) -> &X::Out;
}

macro_rules! has_rel {
    ([$($A:ident),+] $X:ident $I:ident $n:tt) => {
        impl<T: Record, $($A: Relation<T>),+> HasRel<T, $X, idx::$I> for ($($A,)+) {
            fn get(data: &Self::Data) -> &<$X as Relation<T>>::Out {
                &data.$n
            }
        }
    };
}

has_rel!([A] A I0 0);
has_rel!([A, B] A I0 0);
has_rel!([A, B] B I1 1);
has_rel!([A, B, C] A I0 0);
has_rel!([A, B, C] B I1 1);
has_rel!([A, B, C] C I2 2);
has_rel!([A, B, C, D] A I0 0);
has_rel!([A, B, C, D] B I1 1);
has_rel!([A, B, C, D] C I2 2);
has_rel!([A, B, C, D] D I3 3);
has_rel!([A, B, C, D, E] A I0 0);
has_rel!([A, B, C, D, E] B I1 1);
has_rel!([A, B, C, D, E] C I2 2);
has_rel!([A, B, C, D, E] D I3 3);
has_rel!([A, B, C, D, E] E I4 4);
has_rel!([A, B, C, D, E, F] A I0 0);
has_rel!([A, B, C, D, E, F] B I1 1);
has_rel!([A, B, C, D, E, F] C I2 2);
has_rel!([A, B, C, D, E, F] D I3 3);
has_rel!([A, B, C, D, E, F] E I4 4);
has_rel!([A, B, C, D, E, F] F I5 5);

/// `Self` is a loaded record that carries relation `X`. Generated accessors such as
/// `post.author()` are bounded on this.
#[diagnostic::on_unimplemented(
    message = "relation `{X}` was not preloaded on `{Self}`",
    label = "`{X}` is not in this value's preloaded relations",
    note = "add `.preload(...)` for this relation to the query, and include it in the `Loaded<..>` type"
)]
pub trait HasRelation<T: Record, X: Relation<T>, I> {
    fn relation(&self) -> &X::Out;
}

impl<T: Record, R: HasRel<T, X, I>, X: Relation<T>, I> HasRelation<T, X, I> for Loaded<T, R> {
    fn relation(&self) -> &X::Out {
        R::get(&self.data)
    }
}

/// A record together with the relations preloaded for it. Derefs to the record.
pub struct Loaded<T: Record, R: RelSet<T>> {
    pub(crate) record: T,
    pub(crate) data: R::Data,
}

impl<T: Record, R: RelSet<T>> Loaded<T, R> {
    pub fn into_inner(self) -> T {
        self.record
    }

    /// Generic accessor; prefer the generated named methods (`post.author()`).
    pub fn get<X: Relation<T>, I>(&self) -> &X::Out
    where
        R: HasRel<T, X, I>,
    {
        R::get(&self.data)
    }
}

impl<T: Record, R: RelSet<T>> Deref for Loaded<T, R> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.record
    }
}

impl<T: Record, R: RelSet<T>> Clone for Loaded<T, R> {
    fn clone(&self) -> Self {
        Self { record: self.record.clone(), data: self.data.clone() }
    }
}

impl<T: Record + fmt::Debug, R: RelSet<T>> fmt::Debug for Loaded<T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Loaded").field("record", &self.record).field("relations", &std::any::type_name::<R>()).finish()
    }
}

/// Serializes as the record's fields plus one entry per relation, so templates see
/// `post.title` and `post.author.name`.
impl<T: Record, R: RelSet<T>> Serialize for Loaded<T, R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        struct Rels<'a, T: Record, R: RelSet<T>>(&'a R::Data, PhantomData<fn() -> T>);

        impl<T: Record, R: RelSet<T>> Serialize for Rels<'_, T, R> {
            fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(None)?;
                R::serialize_data(self.0, &mut map)?;
                map.end()
            }
        }

        #[derive(Serialize)]
        struct Flat<'a, A: Serialize, B: Serialize> {
            #[serde(flatten)]
            record: &'a A,
            #[serde(flatten)]
            relations: B,
        }

        Flat { record: &self.record, relations: Rels::<T, R>(&self.data, PhantomData) }.serialize(serializer)
    }
}

/// Loader behind generated `belongs_to` relations.
pub async fn load_belongs_to<O, T, N, F>(owners: &[O], fk: F, db: &Db) -> Result<Vec<N::Row>>
where
    O: Record,
    T: Record,
    N: RelSet<T>,
    F: Fn(&O) -> Id<T> + Send + Sync,
{
    if owners.is_empty() {
        return Ok(vec![]);
    }
    let mut ids: Vec<Id<T>> = owners.iter().map(&fk).collect();
    ids.sort();
    ids.dedup();
    let rows = Query::<T, N>::new().filter(Column::<T, Id<T>>::new("id").is_in(ids)).all(db).await?;
    let by_id: HashMap<Id<T>, N::Row> = rows.into_iter().map(|row| (N::record(&row).id(), row)).collect();
    owners
        .iter()
        .map(|owner| {
            let id = fk(owner);
            by_id.get(&id).cloned().ok_or_else(|| {
                Error::Integrity(format!("{} {} points at missing {} {}", O::TABLE, owner.id(), T::TABLE, id))
            })
        })
        .collect()
}

/// Loader behind generated `belongs_to` relations whose foreign key is `Option<Id<T>>`.
/// Skips the query entirely when every key is empty.
pub async fn load_belongs_to_optional<O, T, N, F>(owners: &[O], fk: F, db: &Db) -> Result<Vec<Option<N::Row>>>
where
    O: Record,
    T: Record,
    N: RelSet<T>,
    F: Fn(&O) -> Option<Id<T>> + Send + Sync,
{
    let mut ids: Vec<Id<T>> = owners.iter().filter_map(&fk).collect();
    if ids.is_empty() {
        return Ok(vec![None; owners.len()]);
    }
    ids.sort();
    ids.dedup();
    let rows = Query::<T, N>::new().filter(Column::<T, Id<T>>::new("id").is_in(ids)).all(db).await?;
    let by_id: HashMap<Id<T>, N::Row> = rows.into_iter().map(|row| (N::record(&row).id(), row)).collect();
    owners
        .iter()
        .map(|owner| match fk(owner) {
            None => Ok(None),
            Some(id) => by_id.get(&id).cloned().map(Some).ok_or_else(|| {
                Error::Integrity(format!("{} {} points at missing {} {}", O::TABLE, owner.id(), T::TABLE, id))
            }),
        })
        .collect()
}

/// A child's foreign key to `O`: `Id<O>`, or `Option<Id<O>>` when the child may belong to
/// something else instead (`#[record(one_of(..))]`).
pub trait ForeignKey<O> {
    fn key(&self) -> Option<Id<O>>;
}

impl<O> ForeignKey<O> for Id<O> {
    fn key(&self) -> Option<Id<O>> {
        Some(*self)
    }
}

impl<O> ForeignKey<O> for Option<Id<O>> {
    fn key(&self) -> Option<Id<O>> {
        *self
    }
}

/// Loader behind generated `has_many` relations.
pub async fn load_has_many<O, T, N, F>(
    owners: &[O],
    fk_column: Column<T, Id<O>>,
    fk: F,
    db: &Db,
) -> Result<Vec<Vec<N::Row>>>
where
    O: Record,
    T: Record,
    N: RelSet<T>,
    F: Fn(&T) -> Option<Id<O>> + Send + Sync,
{
    if owners.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Id<O>> = owners.iter().map(Record::id).collect();
    let rows = Query::<T, N>::new().filter(fk_column.is_in(ids)).all(db).await?;
    let mut groups: HashMap<Id<O>, Vec<N::Row>> = HashMap::new();
    for row in rows {
        if let Some(owner) = fk(N::record(&row)) {
            groups.entry(owner).or_default().push(row);
        }
    }
    Ok(owners.iter().map(|owner| groups.get(&owner.id()).cloned().unwrap_or_default()).collect())
}

/// Loader behind generated `has_one` relations: the child pointing at each owner, if any.
/// Should several, the oldest (lowest id) wins; a unique index on the key prevents that.
pub async fn load_has_one<O, T, N, F>(
    owners: &[O],
    fk_column: Column<T, Id<O>>,
    fk: F,
    db: &Db,
) -> Result<Vec<Option<N::Row>>>
where
    O: Record,
    T: Record,
    N: RelSet<T>,
    F: Fn(&T) -> Option<Id<O>> + Send + Sync,
{
    if owners.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Id<O>> = owners.iter().map(Record::id).collect();
    let rows =
        Query::<T, N>::new().filter(fk_column.is_in(ids)).order(Column::<T, Id<T>>::new("id").asc()).all(db).await?;
    let mut by_owner: HashMap<Id<O>, N::Row> = HashMap::new();
    for row in rows {
        if let Some(owner) = fk(N::record(&row)) {
            by_owner.entry(owner).or_insert(row);
        }
    }
    Ok(owners.iter().map(|owner| by_owner.get(&owner.id()).cloned()).collect())
}
