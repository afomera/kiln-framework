// Path helpers are typed by their `Param`: an integer id route won't take a string.
mod show {
    use kiln::prelude::*;

    #[action(GET "/users/{id}")]
    pub async fn show(id: Param<i64>) -> String {
        id.to_string()
    }
}

fn main() {
    let _ = show::path("ada");
}
