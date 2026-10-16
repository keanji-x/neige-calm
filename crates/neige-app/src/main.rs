#[tokio::main]
async fn main() -> anyhow::Result<()> {
    neige_app::run_cli().await
}
