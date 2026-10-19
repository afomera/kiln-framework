//! Policies: who may do what, enforced with proof types.
//!
//! ```ignore
//! impl Allows<Update, Post> for User {
//!     fn allows(&self, post: &Post) -> bool { post.author_id == self.id }
//! }
//!
//! let post = user.authorize::<Update, _>(post)?;          // Authorized<Update, Post>, or 403
//! ops::update_post(&ctx, post, input).await?;             // ops require the proof
//! ```

use std::{fmt, marker::PhantomData, ops::Deref};

use crate::{Error, Loaded, Record, RelSet, Result};

/// `Self` may perform action `A` on `R`.
pub trait Allows<A, R: ?Sized> {
    fn allows(&self, record: &R) -> bool;
}

/// Policies written for a record also cover it with relations preloaded.
impl<A, T: Record, Rs: RelSet<T>, U: Allows<A, T>> Allows<A, Loaded<T, Rs>> for U {
    fn allows(&self, record: &Loaded<T, Rs>) -> bool {
        U::allows(self, record)
    }
}

/// A proof about this *access value* covers writes to the record `R` (or to each record in
/// a tuple). For apps whose policies check a resolved access value rather than the record:
/// when permissions need database lookups (memberships, teams, grants), the action resolves
/// `RepoAccess` asynchronously, the policy is `impl Allows<Write, RepoAccess> for User`, and
/// ops that change the repository's rows take `Authorized<Write, RepoAccess>`.
///
/// ```ignore
/// // src/policies/repository.rs, next to the access type
/// impl Covers<(Repository, RepositoryCollaborator, RepositoryInvitation)> for RepoAccess {}
///
/// pub async fn invite(ctx: &Ctx, repo: Authorized<Admin, RepoAccess>, ..) -> Result<RepositoryInvitation> {
///     RepositoryInvitation::insert(..).run(&ctx.db).await     // proven by the `RepoAccess` proof
/// }
/// ```
///
/// A marker for `kiln check`: it makes each covered record policy-guarded, so a function that
/// writes one must take `Authorized<_, R>` or `Authorized<_, X>` for an `X` that covers it
/// (K0301). Declare it in src/policies/.
pub trait Covers<R: ?Sized> {}

pub struct Create;
pub struct Read;
pub struct Update;
pub struct Delete;

/// Proof that a policy allowed action `A` on `R`. The only way to get one is
/// [`Authorize::authorize`], so an operation that takes `Authorized<Update, Post>` can't
/// be called without the check.
pub struct Authorized<A, R> {
    record: R,
    _action: PhantomData<fn() -> A>,
}

impl<A, R> Authorized<A, R> {
    pub fn into_inner(self) -> R {
        self.record
    }
}

impl<A, R> Deref for Authorized<A, R> {
    type Target = R;

    fn deref(&self) -> &R {
        &self.record
    }
}

impl<A, R: fmt::Debug> fmt::Debug for Authorized<A, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Authorized<{}>({:?})", short_type_name::<A>(), self.record)
    }
}

/// The system acting on its own behalf (a job, a schedule, a seed), with no user to
/// check: grants `A` on `record` unconditionally. Every use is a deliberate decision you
/// can search for; `kiln check` refuses it in actions, which always have a viewer to
/// authorize (K0304).
///
/// ```ignore
/// ops::archive_post(ctx, policy::as_system::<Update, _>(draft)).await?;
/// ```
pub fn as_system<A, R>(record: R) -> Authorized<A, R> {
    Authorized { record, _action: PhantomData }
}

/// Checks policies. Implemented for every record, so `user.authorize::<Update, _>(post)`
/// works on a `User` and on a `Current<User>`.
pub trait Authorize: Sized {
    /// `Ok(proof)` when the policy allows it, otherwise `Error::Forbidden` (403).
    fn authorize<A, R>(&self, record: R) -> Result<Authorized<A, R>>
    where
        Self: Allows<A, R>,
    {
        if <Self as Allows<A, R>>::allows(self, &record) {
            Ok(Authorized { record, _action: PhantomData })
        } else {
            Err(Error::Forbidden { action: short_type_name::<A>(), record: short_type_name::<R>() })
        }
    }

    /// For showing or hiding UI: `user.can::<Update, _>(&post)`.
    fn can<A, R>(&self, record: &R) -> bool
    where
        Self: Allows<A, R>,
    {
        <Self as Allows<A, R>>::allows(self, record)
    }
}

impl<T: Record> Authorize for T {}

/// `blog::records::post::Post` → `Post`; also strips paths inside generics.
fn short_type_name<T>() -> String {
    let full = std::any::type_name::<T>();
    let mut out = String::with_capacity(full.len());
    let mut segment = String::new();
    for c in full.chars() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            segment.push(c);
        } else {
            out.push_str(segment.rsplit("::").next().unwrap_or(&segment));
            segment.clear();
            out.push(c);
        }
    }
    out.push_str(segment.rsplit("::").next().unwrap_or(&segment));
    out
}
