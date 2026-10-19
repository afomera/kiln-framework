use kiln::{policy::Update, prelude::*};

use crate::records::{Bookmark, bookmark};

/// Pins the `pinned` bookmarks among `bookmarks` and unpins the rest. Returns how many
/// changed.
pub async fn pin_bookmarks(
    ctx: &Ctx,
    bookmarks: Vec<Authorized<Update, Bookmark>>,
    pinned: &[Id<Bookmark>],
) -> Result<usize> {
    let changed: Vec<(Id<Bookmark>, bool)> =
        bookmarks.iter().filter(|b| b.pinned != pinned.contains(&b.id)).map(|b| (b.id, !b.pinned)).collect();
    ctx.db
        .transaction(|tx| async move {
            for (id, pin) in &changed {
                Bookmark::update(*id).set(bookmark::pinned, *pin).run(&tx).await?;
            }
            Ok(changed.len())
        })
        .await
}
