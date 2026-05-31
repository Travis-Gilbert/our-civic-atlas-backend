//! Rust gRPC client for the search + embedding sidecar surfaces.
//!
//! Two services are exposed via a single `TheseusClient` (they share one
//! gRPC channel), but they are dialed for different concerns and the
//! target hosts differ:
//!
//!   - `theseus_search.v1.SearchService` -- the canonical search
//!     orchestrator (search / gap-walk / source-pair / provenance). The
//!     `search()` client now dials a Rust-native SearchService at
//!     `THEOREM_SEARCH_URL` (the Django bridge is NOT the intended host;
//!     it remains only as a temporary fallback until the Rust endpoint
//!     stands up). Sibling product to the harness and to RustyRed,
//!     vendored from theorem-protos.
//!   - `theseus_bridge.v1.TheseusBridge` -- spacetime topics, embeddings,
//!     artifact ingest. The `bridge()` client keeps dialing
//!     `THESEUS_BRIDGE_URL` (embedding hydration is a separate concern
//!     from search). This is currently served by the Python sidecar
//!     (Index-API/apps/notebook/grpc/bridge_server.py).
//!
//! The `connect()` single-channel dual-service mechanics are unchanged;
//! only the prose about which host answers which surface has been
//! corrected so the next reader is not told the Django bridge is the
//! search host.

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
