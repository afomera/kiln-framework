//! App settings from the environment and `config/secrets.enc`. See AGENTS.md.

use kiln::prelude::*;

#[derive(Config, Debug)]
pub struct Config {
    /// Email post authors about new comments (`NOTIFY_AUTHORS=false` to stop).
    #[config(default = "true")]
    pub notify_authors: bool,
}
