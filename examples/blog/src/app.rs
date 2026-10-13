//! App settings, used by the server (`src/main.rs`) and by `#[kiln::test]`.

use kiln::{App, Result, config::Config as _};

use crate::{actions::sessions, config::Config, records::User, schedule, views::current_user::CurrentUser};

pub fn configure(app: App) -> Result<App> {
    // People sign in as `User`; templates read the allowlisted `current_user`.
    Ok(app
        .config(Config::load()?)
        .schedule(schedule::schedule)
        .signs_in::<User, _>(sessions::new::path(), CurrentUser::of))
}
