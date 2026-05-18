use std::env;

use civic_atlas_server::{
    http_router, parse_addr, AtlasState, CivicAtlasGrpcService, SpacetimeAtlasGrpcService,
};
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasServiceServer,
    spacetime_atlas_service_server::SpacetimeAtlasServiceServer,
};
use tonic::transport::Server;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "civic_atlas_server=info,tower_http=info".into()),
        )
        .init();

    let state = AtlasState::from_env()?;
    let http_addr = parse_addr(env::var("CIVIC_ATLAS_HTTP_ADDR").ok(), "127.0.0.1:4001")?;
    let grpc_addr = parse_addr(env::var("CIVIC_ATLAS_GRPC_ADDR").ok(), "127.0.0.1:50051")?;

    let http_state = state.clone();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .expect("HTTP listener binds");
        info!(%http_addr, "starting Axum HTTP routes");
        axum::serve(listener, http_router(http_state))
            .await
            .expect("HTTP server runs");
    });

    info!(%grpc_addr, "starting tonic gRPC server");
    Server::builder()
        .accept_http1(true)
        .layer(tonic_web::GrpcWebLayer::new())
        .add_service(CivicAtlasServiceServer::new(CivicAtlasGrpcService::new(
            state.clone(),
        )))
        .add_service(SpacetimeAtlasServiceServer::new(
            SpacetimeAtlasGrpcService::new(state),
        ))
        .serve(grpc_addr)
        .await?;

    Ok(())
}
