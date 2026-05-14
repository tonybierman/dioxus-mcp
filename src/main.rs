use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    dioxus_mcp::run(dioxus_mcp::Cli::parse()).await
}
