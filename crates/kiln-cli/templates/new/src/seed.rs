use kiln::prelude::*;

use crate::records::*;

/// Sample data for development (`kiln db seed`). Does nothing if there are already users.
/// `ctx` is the app as `src/app.rs` configures it: `ctx.db`, services (`ctx.get::<T>()`),
/// storage (`ctx.attach(..)`) and config.
pub async fn seed(ctx: &Ctx) -> Result {
    let db = &ctx.db;
    if User::query().exists(db).await? {
        return Ok(());
    }
    // Two users, so permission rules can be tried by hand.
    User::insert(NewUser { name: "Ada Lovelace".into(), email: "ada@example.com".into() }).run(db).await?;
    User::insert(NewUser { name: "Grace Hopper".into(), email: "grace@example.com".into() }).run(db).await?;
    Ok(())
}
