//! The MCP server: a thin rmcp wrapper over `reconstruct::run_reconstruct`.
//!
//! Two tools:
//! - `reconstruct` (primary): runs the pipeline and returns the manifest +
//!   provenance inline as JSON, plus the glTF scene IR as a resource link
//!   (file:// URI + content hash + byte size). Never inlines mesh bytes.
//! - `engine_info`: a small descriptor of the engine version, the tier ladder,
//!   and the renderer-agnostic (glTF out) contract.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, RawResource, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};

use crate::reconstruct::{run_reconstruct, ReconstructInput, TIER_LADDER};

/// The reconstruction MCP server. Stateless: every `reconstruct` call builds
/// its own in-memory repository and renderer, so the server holds no Postgres,
/// Theseus, or GPU handles.
#[derive(Clone)]
pub struct ReconstructionMcpServer {
    tool_router: ToolRouter<Self>,
}

impl ReconstructionMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for ReconstructionMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl ReconstructionMcpServer {
    /// Reconstruct a (often demolished) building from structured evidence and
    /// constraints, returning the produced scene IR so any renderer can draw
    /// it. The glTF asset is returned by reference (a resource link); the
    /// manifest and provenance record are returned inline as JSON.
    #[tool(
        name = "reconstruct",
        description = "Run the Civic Atlas procedural reconstruction pipeline for one building \
        from structured evidence (Sanborn sheets, archival photos, GIS features, directory \
        entries, free text) plus constraints (tenant, parcel id, year, footprint). Returns the \
        scene IR: a glTF (GLB) asset by reference (resource link with file:// URI + sha256 content \
        hash + byte size, never inline mesh bytes), plus the asset manifest and the provenance \
        record inline as JSON. Rendering the returned glTF is the caller's responsibility."
    )]
    pub async fn reconstruct(
        &self,
        Parameters(input): Parameters<ReconstructInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let output = run_reconstruct(input)
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;

        // Inline JSON: the manifest + provenance are small and are what a
        // calling agent reasons over. Serialize the whole ReconstructOutput
        // (which carries the gltf reference too) as the structured payload.
        let summary = serde_json::to_value(&output).map_err(|error| {
            ErrorData::internal_error(format!("serializing reconstruct output: {error}"), None)
        })?;

        let summary_content = Content::json(&summary).map_err(|error| {
            ErrorData::internal_error(format!("encoding reconstruct output: {error}"), error.data)
        })?;

        // The glTF scene IR as an MCP resource link: file:// URI, content hash
        // in the description, byte size for host display. This respects MCP
        // payload limits by referencing the asset rather than embedding it.
        let gltf_link = Content::resource_link(RawResource {
            uri: output.gltf.file_uri.clone(),
            name: format!("{}.glb", output.spec_id),
            title: Some(format!(
                "Scene IR (glTF binary) for {} v{}",
                output.spec_id, output.spec_version
            )),
            description: Some(format!(
                "Procedural massing GLB. content_hash={}, render_tier={}, status={}",
                output.gltf.content_hash,
                output.render_tier.as_deref().unwrap_or("unknown"),
                output.status
            )),
            mime_type: Some("model/gltf-binary".to_string()),
            size: Some(output.gltf.size_bytes.min(u32::MAX as u64) as u32),
            icons: None,
            meta: None,
        });

        let mut result = CallToolResult::success(vec![summary_content, gltf_link]);
        // Also expose the output as structured_content so hosts that prefer the
        // typed channel get it without reparsing the text block.
        result.structured_content = Some(summary);
        Ok(result)
    }

    /// Describe the engine: version, the evidence-graded render-tier ladder, and
    /// the renderer-agnostic (glTF out) contract.
    #[tool(
        name = "engine_info",
        description = "Return a descriptor of the reconstruction engine: crate version, the \
        evidence-graded render-tier ladder, and the fact that the engine is renderer-agnostic \
        (it emits glTF scene IR; drawing it is the caller's problem)."
    )]
    pub async fn engine_info(&self) -> Result<CallToolResult, ErrorData> {
        let info = serde_json::json!({
            "engine": "civic-atlas-reconstruction-engine",
            "engineVersion": env!("CARGO_PKG_VERSION"),
            "renderer": "civic-atlas-renderer (Scene Foundry)",
            "prior": "BlockCoherentPriorModel (procedural Pairformer-adapter heuristic); \
                the learned Pairformer prior slots in via the PriorModel seam without changing \
                this tool surface",
            "embeddings": "ZeroEmbeddingProvider (honest missing-upstream zeros; no Theseus)",
            "standalone": "no Postgres, no Theseus gRPC, no GPU; runs from in-memory evidence",
            "tierLadder": TIER_LADDER,
            "output": "glTF (GLB) scene IR returned by reference; renderer-agnostic",
        });
        Ok(CallToolResult::structured(info))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ReconstructionMcpServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (InitializeResult) and Implementation are #[non_exhaustive]:
        // build via their public constructors, then set the fields we own.
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new(
            "civic-atlas-reconstruction-mcp",
            env!("CARGO_PKG_VERSION"),
        );
        info.instructions = Some(
            "Call `reconstruct` with structured building evidence and constraints to run the \
            Civic Atlas procedural reconstruction pipeline. It returns the asset manifest and \
            provenance record inline as JSON, plus the produced glTF (GLB) scene IR as a \
            resource link (file:// URI). Fetch that resource to render; rendering is your \
            responsibility. Call `engine_info` for the tier ladder and engine descriptor."
                .to_string(),
        );
        info
    }
}
