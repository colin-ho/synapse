#[tokio::main]
async fn main() -> anyhow::Result<()> {
    synapse::daemon::run().await
}
