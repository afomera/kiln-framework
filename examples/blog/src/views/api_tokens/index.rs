use kiln::{auth::ApiToken, prelude::*};

use crate::forms::ApiTokenForm;

#[derive(View)]
#[view(template = "api_tokens/index")]
pub struct Index {
    pub tokens: Vec<ApiToken>,
    pub form: FormState<ApiTokenForm>,
}
