#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    kiln::Cli::new(env!("CARGO_MANIFEST_DIR")).seed(blog::seed).configure(blog::app::configure).run().await;
}
