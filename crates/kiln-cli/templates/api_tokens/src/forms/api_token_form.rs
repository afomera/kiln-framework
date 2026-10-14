use kiln::prelude::*;

/// The scopes a token can have. API routes name the one they need,
/// `#[action(.., pipeline = Api, scope = "write")]`; cookie sign-ins have every scope. Add yours (`repo:read`, `repo:write`, ..) here and in
/// the hint below.
pub const API_SCOPES: &[&str] = &["read", "write"];

#[derive(Form)]
pub struct ApiTokenForm {
    #[form(hint = "What it's for, like \"Laptop\" or \"CI deploys\".")]
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[form(
        hint = "Space-separated, from: read write. Leave empty for a token that works only on routes open to any token."
    )]
    #[validate(length(max = 500))]
    pub scopes: Option<String>,
    #[form(label = "Expires in (days)", hint = "Leave empty for a token that doesn't expire.")]
    #[validate(range(min = 1, max = 3650))]
    pub expires_in_days: Option<i64>,
}

impl ApiTokenForm {
    /// The scopes asked for, or the first one that isn't in [`API_SCOPES`].
    pub fn scope_list(&self) -> Result<Vec<&str>, String> {
        let asked = self.scopes.as_deref().unwrap_or_default();
        let scopes: Vec<&str> =
            asked.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).collect();
        match scopes.iter().find(|s| !API_SCOPES.contains(s)) {
            Some(unknown) => Err(format!("has an unknown scope `{unknown}` (choose from: {})", API_SCOPES.join(" "))),
            None => Ok(scopes),
        }
    }
}
