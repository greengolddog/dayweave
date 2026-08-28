use std::process::ExitCode;

use dayweave_api::{AppState, config::Config, healthcheck::local_healthcheck, http::router};
use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("healthcheck") => {
            return match local_healthcheck(std::time::Duration::from_secs(3)).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("healthcheck failed: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Some(command) => {
            eprintln!("unknown command: {command}; expected no command or 'healthcheck'");
            return ExitCode::FAILURE;
        }
        None => {}
    }

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    initialize_tracing(&config);

    let listener = match TcpListener::bind(config.bind_address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, address = %config.bind_address, "failed to bind API listener");
            return ExitCode::FAILURE;
        }
    };
    let state = match AppState::from_config(&config).await {
        Ok(state) => state,
        Err(error) => {
            tracing::error!(%error, "failed to initialize persistent application state");
            return ExitCode::FAILURE;
        }
    };
    state.readiness.set_ready(true);
    tracing::info!(address = %config.bind_address, "DayWeave API listening");

    let readiness = state.readiness.clone();
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    readiness.set_ready(false);

    if let Err(error) = result {
        tracing::error!(%error, "API server stopped with an error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn initialize_tracing(config: &Config) {
    let registry = tracing_subscriber::registry().with(
        EnvFilter::try_new(&config.log_filter)
            .unwrap_or_else(|_| EnvFilter::new("dayweave_api=info,tower_http=info")),
    );
    if config.json_logs {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

async fn shutdown_signal() {
    let control_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
