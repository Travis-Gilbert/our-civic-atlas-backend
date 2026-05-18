use civic_atlas_types::theseus_bridge::v1::theseus_bridge_client::TheseusBridgeClient;
use tonic::transport::{Channel, Endpoint};

#[derive(Clone)]
pub struct TheseusClient {
    inner: TheseusBridgeClient<Channel>,
}

impl TheseusClient {
    pub async fn connect(url: impl AsRef<str>) -> Result<Self, tonic::transport::Error> {
        let endpoint = Endpoint::from_shared(url.as_ref().to_string())?;
        let channel = endpoint.connect().await?;
        Ok(Self {
            inner: TheseusBridgeClient::new(channel),
        })
    }

    pub fn inner(&mut self) -> &mut TheseusBridgeClient<Channel> {
        &mut self.inner
    }
}
