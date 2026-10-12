#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    kiln::Cli::new(env!("CARGO_MANIFEST_DIR"))
        .seed(@@crate@@::seed)
        .configure(@@crate@@::app::configure)
        .run()
        .await;
}
