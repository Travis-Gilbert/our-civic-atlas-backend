//! Rust gRPC client for the Theseus sidecar.
//!
//! The Theseus sidecar process (Index-API/apps/notebook/grpc/bridge_server.py)
//! hosts two services on the same port:
//!
//!   - `theseus_bridge.v1.TheseusBridge` -- spacetime topics, embeddings,
//!     artifact ingest. Theseus-internal infrastructure surfaces.
//!   - `theseus_search.v1.SearchService` -- the canonical search
//!     orchestrator (search / gap-walk / source-pair / provenance).
//!     Sibling product to the harness and to RustyRed, vendored from
//!     theorem-protos.
//!
//! This crate exposes both clients via a single `TheseusClient` so the
//! civic-atlas-server (and any future caller) can dial one URL and call
//! either service. They share the same gRPC channel.

use civic_atlas_types::theseus_bridge::v1::theseus_bridge_client::TheseusBridgeClient;
use civic_atlas_types::theseus_search::v1::search_service_client::SearchServiceClient;
use tonic::transport::{Channel, Endpoint};

#[derive(Clone)]
pub struct TheseusClient {
    bridge: TheseusBridgeClient<Channel>,
    search: SearchServiceClient<Channel>,
}

impl TheseusClient {
    /// Dial the Theseus sidecar URL. Both clients share the channel.
    pub async fn connect(url: impl AsRef<str>) -> Result<Self, tonic::transport::Error> {
        let endpoint = Endpoint::from_shared(url.as_ref().to_string())?;
        let channel = endpoint.connect().await?;
        Ok(Self {
            bridge: TheseusBridgeClient::new(channel.clone()),
            search: SearchServiceClient::new(channel),
        })
    }

    /// Access the `theseus_bridge.v1.TheseusBridge` client surface.
    /// Used for spacetime topics, embeddings, artifact ingest.
    pub fn bridge(&mut self) -> &mut TheseusBridgeClient<Channel> {
        &mut self.bridge
    }

    /// Access the `theseus_search.v1.SearchService` client surface.
    /// Used for civic research search, gap-walk, source-pair, provenance.
    pub fn search(&mut self) -> &mut SearchServiceClient<Channel> {
        &mut self.search
    }
}
