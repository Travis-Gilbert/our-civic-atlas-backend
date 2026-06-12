//! Civic Atlas reconstruction engine + Scene Foundry renderer, exposed as a
//! Model Context Protocol (MCP) server over stdio.
//!
//! This crate is a WRAP, not a new engine. It puts exactly one primary tool,
//! `reconstruct`, in front of the existing, working code:
//!
//! - `civic_atlas_reconstruction_engine::run_full_pipeline` runs the eight
//!   procedural reconstruction stages (evidence assembly, direct extraction,
//!   block subgraph, embedding hydration, prior inference, merge, asset
//!   generation).
//! - `civic_atlas_renderer::SceneFoundryRenderer` is the `AssetGenerator`: it
//!   writes a real `massing.glb` plus a `provenance.json` to an asset store
//!   and returns a manifest with completed status and sha256 content hashes.
//!
//! The product framing for this surface: the contract is
//! `reconstruct(domain, evidence, constraints)` returning scene IR as a
//! resource (glTF or SceneDirective JSON, never inline meshes, because MCP
//! payload limits make inline mesh bytes a non-starter), and rendering is then
//! anyone's problem. We honor that literally: the GLB is returned as an MCP
//! resource link (a `file://` URI plus the content hash and byte size), never
//! as inline mesh bytes in the tool result. The manifest and the provenance
//! record are returned inline as JSON, since they are small and are the part a
//! calling agent reasons over.
//!
//! Standalone by construction. The tool body assembles an
//! `InMemoryRepository` from the caller's structured evidence, so there is no
//! Postgres, no Theseus gRPC bridge, and no GPU in the loop. Embeddings come
//! from `ZeroEmbeddingProvider` (honest "missing upstream" zeros) and the prior
//! comes from `BlockCoherentPriorModel` (the procedural Pairformer-adapter
//! heuristic). The learned Pairformer prior slots in later through the same
//! `PriorModel` seam without changing this tool surface.
//!
//! The MCP API surface is grounded in rmcp 1.7.0: the `#[tool_router]` /
//! `#[tool]` / `#[tool_handler]` macros declare the tool router and handler,
//! `Parameters<T>` extracts the (Deserialize + JsonSchema) input, the tool
//! returns `Result<CallToolResult, ErrorData>`, and the server runs over
//! `rmcp::transport::stdio` via `ServiceExt::serve(stdio()).await?.waiting()`.

mod reconstruct;
mod server;

use anyhow::Context as _;
use rmcp::{transport::stdio, ServiceExt};

use server::ReconstructionMcpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Log to stderr only: stdout is the MCP JSON-RPC channel and must stay
    // clean. EnvFilter honors RUST_LOG; default to info.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tracing::info!("civic-atlas-reconstruction-mcp starting on stdio");

    let service = ReconstructionMcpServer::new()
        .serve(stdio())
        .await
        .context("starting MCP server on stdio")?;

    service.waiting().await.context("MCP server run loop")?;

    Ok(())
}
