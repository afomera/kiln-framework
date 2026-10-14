//! Typed joins: `Repository::query().join(repository::namespace)` lets the query filter,
//! order and select on `Namespace` columns while still returning `Repository` rows.
//!
//! A query carries the tables it joins in its type (`Query<T, R, J>`, `J` a tuple of
//! [`Inner`]/[`Left`] markers), so a column of a table it doesn't join is a compile error,
//! just like a column of another table is without joins.

use std::marker::PhantomData;

use crate::{Record, rel::idx};

/// A relation a query can join: `belongs_to` and `has_one`, which match at most one row
/// per record, so a joined query still returns each record once. `#[derive(Record)]`
/// implements it on their markers (`repository::namespace`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't be joined: it can match many rows per record",
    label = "only `belongs_to` and `has_one` relations join",
    note = "joining a `has_many` would repeat each record once per child; filter through it with a subquery instead: `post::id.in_query(Comment::query().filter(..).select(comment::post_id))`"
)]
pub trait Join {
    /// The record the relation is declared on.
    type Owner: Record;
    /// The record it leads to, whose table the join adds.
    type Target: Record;
    #[doc(hidden)]
    const ON: JoinOn;
}

/// How a relation's tables line up. Written by `#[derive(Record)]`.
#[doc(hidden)]
pub enum JoinOn {
    /// The owner holds the key: `posts.author_id = users.id`.
    BelongsTo(&'static str),
    /// The target holds the key: `profiles.user_id = users.id`.
    HasOne(&'static str),
}

/// A table joined with [`crate::Query::join`]: its rows always exist.
pub struct Inner<U>(PhantomData<fn() -> U>);

/// A table joined with [`crate::Query::left_join`]: its columns may be NULL.
pub struct Left<U>(PhantomData<fn() -> U>);

/// Where the query's own record sits among its tables. Never written by hand.
pub struct Base;

/// Columns of `Self` may appear in a query for `T` joining `J`: `Self` is `T` or joined.
/// `I` is its position, so the compiler can find it without overlapping impls.
#[diagnostic::on_unimplemented(
    message = "`{Self}` isn't joined into this `{T}` query",
    label = "this is a column of `{Self}`",
    note = "add `.join(..)` with the `belongs_to`/`has_one` relation leading to `{Self}` (e.g. `.join(repository::namespace)` before using `namespace::path`); to filter through a `has_many`, use `.in_query(..)`"
)]
pub trait InScope<T, J, I> {}

/// Like [`InScope`], for reading a column's values (`select`, `count_by`): never a
/// `left_join`ed table, whose columns may be NULL where the column type says otherwise.
#[diagnostic::on_unimplemented(
    message = "can't read `{Self}` columns from this `{T}` query",
    label = "this is a column of `{Self}`",
    note = "read columns of `{T}` or of tables added with `.join(..)`; a `.left_join(..)` table's columns may be NULL, so read those with `db.sql`"
)]
pub trait Selectable<T, J, I> {}

impl<T, J> InScope<T, J, Base> for T {}
impl<T, J> Selectable<T, J, Base> for T {}

macro_rules! joined_at {
    ($I:ident; $($B:ident)*; $($A:ident)*) => {
        impl<T, U, $($B,)* $($A,)*> InScope<T, ($($B,)* Inner<U>, $($A,)*), idx::$I> for U {}
        impl<T, U, $($B,)* $($A,)*> InScope<T, ($($B,)* Left<U>, $($A,)*), idx::$I> for U {}
        impl<T, U, $($B,)* $($A,)*> Selectable<T, ($($B,)* Inner<U>, $($A,)*), idx::$I> for U {}
    };
}

joined_at!(I0;;);
joined_at!(I0;; B);
joined_at!(I1; A;);
joined_at!(I0;; B C);
joined_at!(I1; A; C);
joined_at!(I2; A B;);
joined_at!(I0;; B C D);
joined_at!(I1; A; C D);
joined_at!(I2; A B; D);
joined_at!(I3; A B C;);

/// Type-level append for the joined tables, capped where [`InScope`] has impls.
#[diagnostic::on_unimplemented(
    message = "a query joins at most 4 tables",
    label = "this query already joins `{Self}`",
    note = "filter through further tables with `.in_query(..)`, or write the query with `db.sql(..)`"
)]
pub trait AddJoin<X> {
    type Output;
}

impl<X> AddJoin<X> for () {
    type Output = (X,);
}

impl<A, X> AddJoin<X> for (A,) {
    type Output = (A, X);
}

impl<A, B, X> AddJoin<X> for (A, B) {
    type Output = (A, B, X);
}

impl<A, B, C, X> AddJoin<X> for (A, B, C) {
    type Output = (A, B, C, X);
}

/// The `JOIN` clause for relation `X`.
pub(crate) fn clause<X: Join>(kind: &str) -> String {
    let owner = <X::Owner as Record>::TABLE;
    let target = <X::Target as Record>::TABLE;
    match X::ON {
        JoinOn::BelongsTo(fk) => format!("{kind} JOIN \"{target}\" ON \"{target}\".\"id\" = \"{owner}\".\"{fk}\""),
        // The oldest child only, as preloading picks, so a missing unique index can't
        // repeat the owner's row.
        JoinOn::HasOne(fk) => format!(
            "{kind} JOIN \"{target}\" ON \"{target}\".\"id\" = (SELECT MIN(\"{target}\".\"id\") FROM \"{target}\" WHERE \"{target}\".\"{fk}\" = \"{owner}\".\"id\")"
        ),
    }
}
