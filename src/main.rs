use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use tracing_subscriber::EnvFilter;

mod project;
mod server;
mod state;
mod tools;

use crate::server::DioxusMcp;
use crate::state::State;

#[derive(Debug, Clone, clap::ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

#[derive(Parser, Debug)]
#[command(name = "dioxus-mcp", about = "MCP server for Dioxus projects")]
struct Cli {
    #[arg(long, value_enum, default_value_t = Transport::Stdio)]
    transport: Transport,

    #[arg(long, default_value = "127.0.0.1:8731")]
    bind: String,

    #[arg(long)]
    project_root: Option<PathBuf>,

    #[arg(long, default_value = "info")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let project_root = cli
        .project_root
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let state = Arc::new(State::new(project_root)?);
    tracing::info!(?state.project_root, "dioxus-mcp starting");

    match cli.transport {
        Transport::Stdio => {
            let handler = DioxusMcp::new(state);
            let service = handler.serve(stdio()).await?;
            service.waiting().await?;
        }
        Transport::Http => {
            let bind = cli.bind.clone();
            let svc = TowerToHyperService::new(StreamableHttpService::new(
                {
                    let state = state.clone();
                    move || Ok(DioxusMcp::new(state.clone()))
                },
                LocalSessionManager::default().into(),
                Default::default(),
            ));
            let listener = tokio::net::TcpListener::bind(&bind).await?;
            tracing::info!(%bind, "streamable HTTP listening");
            loop {
                let io = tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    accept = listener.accept() => TokioIo::new(accept?.0),
                };
                let svc = svc.clone();
                tokio::spawn(async move {
                    let _ = Builder::new(TokioExecutor::default())
                        .serve_connection(io, svc)
                        .await;
                });
            }
        }
    }

    Ok(())
}
