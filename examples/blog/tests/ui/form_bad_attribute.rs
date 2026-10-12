// A typo in a form attribute is one clear error, not a cascade from code using the form.
use kiln::prelude::*;

#[derive(Form, Debug)]
pub struct EventForm {
    #[form(tooltip = "When it starts")]
    pub name: String,
}

#[action(POST "/events")]
pub async fn create(form: Form<EventForm>) -> Result<String> {
    let input = form.validate()?;
    Ok(input.name.clone())
}

fn main() {}
