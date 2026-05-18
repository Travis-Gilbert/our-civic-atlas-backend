use std::env;

use civic_atlas_server::{
    corrections::CorrectionGrpcService, http_router, parse_addr,
    reconstruction::ReconstructionGrpcService, AtlasState, CivicAtlasGrpcService,
    SpacetimeAtlasGrpcService,
};
use civic_atlas_types::civic_atlas::v1::{
    civic_atlas_service_server::CivicAtlasServiceServer,
    correction_service_server::CorrectionServiceServer,
    reconstruction_service_server::ReconstructionServiceServer,
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
    state.run_migrations().await?;
    // Address resolution order for HTTP:
    //   1. CIVIC_ATLAS_HTTP_ADDR  — explicit "host:port" wins (local dev)
    //   2. PORT (e.g. Railway)    — bind to 0.0.0.0:$PORT so the platform
    //                                load balancer can route traffic
    //   3. 127.0.0.1:4001         — local default
    let http_addr_str = env::var("CIVIC_ATLAS_HTTP_ADDR")
        .ok()
        .or_else(|| env::var("PORT").ok().map(|p| format!("0.0.0.0:{p}")));
    let http_addr = parse_addr(http_addr_str, "127.0.0.1:4001")?;

    // gRPC stays on its own port. Railway can either expose it via a
    // separate service or keep it on the private network for the Node
    // sidecar to reach. Default 0.0.0.0:50051 binds for both, so a
    // deployed environment that wants gRPC reachable just exposes the
    // port via Railway's networking UI.
    let grpc_addr = parse_addr(
        env::var("CIVIC_ATLAS_GRPC_ADDR").ok(),
        "0.0.0.0:50051",
    )?;

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
            SpacetimeAtlasGrpcService::new(state.clone()),
        ))
        .add_service(ReconstructionServiceServer::new(
            ReconstructionGrpcService::new(state.clone()),
        ))
        .add_service(CorrectionServiceServer::new(CorrectionGrpcService::new(
            state,
        )))
        .serve(grpc_addr)
        .await?;

    Ok(())
}
