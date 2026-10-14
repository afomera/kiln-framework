//! Rate limits, counted in the cache (`kiln_cache`), so they hold across processes with
//! nothing extra to run.
//!
//! ```ignore
//! #[action(GET "/api/repos", pipeline = Api, scope = "repo:read")]
//! pub async fn index(ctx: Ctx, user: Current<User>, limit: RateLimit) -> Result<Json<Vec<RepoJson>>> {
//!     limit.check("api", 5000, SignedDuration::from_hours(1)).await?;     // per user
//!     ..
//! }
//!
//! #[action(POST "/session")]
//! pub async fn create(ctx: Ctx, limit: RateLimit, form: Form<SignInForm>) -> Result<Redirect> {
//!     limit.check("sign_in", 20, SignedDuration::from_mins(5)).await?;      // per IP address
//!     let input = form.validate().or_render(|form| New { form })?;
//!     limit.check_key("sign_in:email", &input.email, 5, SignedDuration::from_mins(15)).await?;
//!     ..
//! }
//! ```
//!
//! Past the limit, the action fails with `Error::RateLimited`: a 429 with `Retry-After`.
//! Windows are fixed (requests per clock-aligned minute, hour, ..), so a client can make up
//! to twice the limit across a window boundary.

use std::convert::Infallible;

use axum::{extract::FromRequestParts, http::request::Parts};
use jiff::SignedDuration;
use sha2::{Digest, Sha256};

use crate::{Ctx, Error, Result, Timestamp, cache::Cache};

/// Rate limits for this request. Take it as an action argument.
pub struct RateLimit {
    cache: Cache,
    ip: String,
}

impl FromRequestParts<Ctx> for RateLimit {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Self::Rejection> {
        Ok(Self { cache: ctx.cache(), ip: crate::app::client_ip(parts) })
    }
}

impl RateLimit {
    /// Allows `max` requests per `per` under `name` for whoever is asking: the signed-in
    /// record when the action takes `Current<..>` (or `Option<Current<..>>`), else the
    /// client's IP address. Past that, `Error::RateLimited` (429 with `Retry-After`).
    pub async fn check(&self, name: &str, max: u32, per: SignedDuration) -> Result<()> {
        let signed_in = crate::session::current_scope()
            .and_then(|scope| scope.cache.signed_in_as.lock().unwrap_or_else(|e| e.into_inner()).clone());
        let who = match signed_in {
            Some(record) => record,
            None if self.ip.is_empty() => "ip:unknown".to_owned(),
            None => format!("ip:{}", self.ip),
        };
        self.hit(name, &who, max, per).await
    }

    /// Like [`RateLimit::check`], counted under a `key` you choose: the email someone is
    /// signing in as, an account, a repository. The key is hashed, so emails never end up
    /// in cache keys.
    pub async fn check_key(&self, name: &str, key: &str, max: u32, per: SignedDuration) -> Result<()> {
        let hashed: String = Sha256::digest(key.as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect();
        self.hit(name, &format!("key:{hashed}"), max, per).await
    }

    async fn hit(&self, name: &str, who: &str, max: u32, per: SignedDuration) -> Result<()> {
        let per = per.as_secs();
        if per < 1 {
            return Err(Error::BadRequest(format!(
                "the `{name}` rate limit's window is under a second; use at least `SignedDuration::from_secs(1)`"
            )));
        }
        let now = Timestamp::now().as_jiff().as_second();
        let window = now.div_euclid(per);
        let retry_after = ((window + 1) * per - now).max(1);
        let key = format!("rate:{name}:{who}:{window}");
        let count = self.cache.increment(&key, 1, SignedDuration::from_secs(retry_after)).await?;
        if count > i64::from(max) {
            return Err(Error::RateLimited { retry_after: retry_after as u64 });
        }
        Ok(())
    }
}
