use std::{future::IntoFuture, net::SocketAddr, process::ExitCode, sync::Arc, time::Duration};

use autoindex_rs::{Cli, Config, build_router, open_state};
use clap::Parser;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("autoindex-rs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dotenv_path = std::env::current_dir()?.join(".env");
    if dotenv_path.try_exists()? {
        dotenvy::from_path(&dotenv_path)?;
    }

    let cli = Cli::parse();
    let config = Config::resolve(cli)?;
    init_tracing(config.log_level.as_str())?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async_main(config))
}

fn init_tracing(level: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::try_new(format!("autoindex_rs={level},tower_http={level}"))?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init()?;
    Ok(())
}

async fn async_main(config: Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let address = SocketAddr::new(config.bind, config.port);
    let state = Arc::new(open_state(config)?);
    let app = build_router(state.clone());
    let listener = TcpListener::bind(address).await?;
    info!(
        address = %listener.local_addr()?,
        directory = %state.config.directory.display(),
        "directory index server started"
    );

    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_sender.send(true);
    });

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_receiver.clone()))
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        () = shutdown_deadline(shutdown_receiver) => {
            error!("graceful shutdown exceeded 10 seconds; closing remaining connections");
        }
    }
    Ok(())
}

async fn wait_for_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

async fn shutdown_deadline(receiver: tokio::sync::watch::Receiver<bool>) {
    wait_for_shutdown(receiver).await;
    tokio::time::sleep(Duration::from_secs(10)).await;
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
