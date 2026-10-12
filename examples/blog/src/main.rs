use kiln::{App, Db};

#[tokio::main]
async fn main() -> kiln::Result {
    tracing_subscriber::fmt::init();
    let db = Db::memory().await?;
    blog::seed(&db).await?;
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    App::new(db).serve(&format!("127.0.0.1:{port}")).await
}
