use external_dns_webhook_timeweb::{
    config::Config,
    provider::Provider,
    timeweb::TimewebClient,
    webhook::{AppState, health_router, provider_router},
};
use std::error::Error;
use tokio::{net::TcpListener, sync::watch};
use tracing::info;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("external-dns-webhook-timeweb: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => tracing_subscriber::EnvFilter::new("info"),
    };
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init()?;

    let config = Config::from_env()?;
    let client = TimewebClient::new(
        config.api_url().clone(),
        config.token(),
        config.http_timeout(),
    )?;
    let provider = Provider::new(client, config.domain_filter().clone());
    let state = AppState::new(provider);
    let provider_listener = TcpListener::bind(config.listen_addr()).await?;
    let health_listener = TcpListener::bind(config.metrics_addr()).await?;

    info!(
        provider_address = %config.listen_addr(),
        health_address = %config.metrics_addr(),
        "webhook servers started"
    );

    serve(state, provider_listener, health_listener).await
}

async fn serve(
    state: AppState,
    provider_listener: TcpListener,
    health_listener: TcpListener,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (shutdown_sender, receiver) = watch::channel(false);
    let provider_server = axum::serve(provider_listener, provider_router(state.clone()))
        .with_graceful_shutdown(wait_for_shutdown(receiver.clone()))
        .into_future();
    let health_server = axum::serve(health_listener, health_router(state.metrics.clone()))
        .with_graceful_shutdown(wait_for_shutdown(receiver))
        .into_future();

    let signal = shutdown_signal(shutdown_sender.clone());
    tokio::pin!(provider_server);
    tokio::pin!(health_server);
    tokio::pin!(signal);

    tokio::select! {
        result = &mut provider_server => {
            let _ = shutdown_sender.send(true);
            result?;
        }
        result = &mut health_server => {
            let _ = shutdown_sender.send(true);
            result?;
        }
        _ = &mut signal => {
            provider_server.await?;
            health_server.await?;
        }
    }
    Ok(())
}

async fn wait_for_shutdown(mut receiver: watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    let _ = receiver.changed().await;
}

async fn shutdown_signal(sender: watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to wait for shutdown signal: {error}");
    }
    let _ = sender.send(true);
}
