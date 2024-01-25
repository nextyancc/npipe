mod global;
mod peer;
mod player;
mod utils;
mod web;

use crate::global::config::GLOBAL_CONFIG;
use crate::peer::Peer;
use anyhow::anyhow;
use np_base::net::server;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::{select, signal};

pub async fn run_tcp_server() -> anyhow::Result<()> {
    let listener = server::bind(GLOBAL_CONFIG.listen_addr.as_str()).await?;
    server::run_server(
        listener,
        || Box::new(Peer::new()),
        |stream: TcpStream| async move { Ok(stream) },
        signal::ctrl_c(),
    )
    .await;
    Ok(())
}

pub async fn run_web_server() -> anyhow::Result<()> {
    let addr = GLOBAL_CONFIG.web_addr.parse::<SocketAddr>();
    return match addr {
        Ok(addr) => web::run_http_server(&addr).await,
        Err(parse_error) => Err(anyhow!(parse_error.to_string())),
    };
}

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    global::init_global().await?;

    let mut result: anyhow::Result<()> = Ok(());

    select! {
        r = run_tcp_server() => { result = r },
        r = run_web_server() => { result = r },
    }

    result
}
