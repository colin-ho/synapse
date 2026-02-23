#[tokio::main]
async fn main() -> anyhow::Result<()> {
    synapse::cli::run().await
}
