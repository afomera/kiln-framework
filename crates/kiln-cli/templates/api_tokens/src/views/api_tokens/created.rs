use kiln::{auth::ApiToken, prelude::*};

#[derive(View)]
#[view(template = "api_tokens/created")]
pub struct Created {
    pub token: ApiToken,
    /// The token itself, shown on this page only.
    pub secret: String,
}
