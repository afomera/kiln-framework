#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    kiln::Cli::new(env!("CARGO_MANIFEST_DIR"))
        // Release builds carry templates, assets and migrations inside the binary.
        .files(kiln::embedded_files!())
        .seed(@@crate@@::seed)
        .configure(@@crate@@::app::configure)
        .run()
        .await;
}
