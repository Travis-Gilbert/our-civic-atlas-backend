use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use civic_atlas_types::civic_atlas::v1::TenantContext;
use http::{header::HeaderName, Request, Response, StatusCode};
use thiserror::Error;
use tower::{Layer, Service};

pub const TENANT_HEADER: &str = "x-tenant-id";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TenantId(String);

impl TenantId {
    pub fn parse(value: impl Into<String>) -> Result<Self, TenantResolutionError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(TenantResolutionError::MissingTenant);
        }
        if !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':'))
        {
            return Err(TenantResolutionError::InvalidTenant);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TenantResolutionError {
    #[error("tenant_id is required")]
    MissingTenant,
    #[error("tenant_id contains unsupported characters")]
    InvalidTenant,
}

pub fn require_tenant_context(
    context: Option<&TenantContext>,
) -> Result<TenantId, TenantResolutionError> {
    let tenant_id = context
        .map(|ctx| ctx.tenant_id.as_str())
        .unwrap_or_default();
    TenantId::parse(tenant_id)
}

#[derive(Clone, Debug, Default)]
pub struct RequireTenantLayer;

impl<S> Layer<S> for RequireTenantLayer {
    type Service = RequireTenantService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequireTenantService { inner }
    }
}

#[derive(Clone, Debug)]
pub struct RequireTenantService<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RequireTenantService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let tenant_header = HeaderName::from_static(TENANT_HEADER);
        let tenant_ok = request
            .headers()
            .get(tenant_header)
            .and_then(|value| value.to_str().ok())
            .map(TenantId::parse)
            .transpose()
            .map(|tenant| tenant.is_some())
            .unwrap_or(false);

        if !tenant_ok {
            let response = Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(ResBody::default())
                .expect("tenant rejection response builds");
            return Box::pin(async move { Ok(response) });
        }

        let mut inner = self.inner.clone();
        Box::pin(async move { inner.call(request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_rejects_missing_tenant() {
        assert_eq!(
            require_tenant_context(None),
            Err(TenantResolutionError::MissingTenant)
        );
    }

    #[test]
    fn context_accepts_slug_tenant() {
        let context = TenantContext {
            tenant_id: "flint".to_string(),
            atlas_node_id: "atlas:flint".to_string(),
            metadata: Default::default(),
        };

        let tenant = require_tenant_context(Some(&context)).unwrap();

        assert_eq!(tenant.as_str(), "flint");
    }
}
