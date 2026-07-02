use super::{ocr_via, ocr_via_stream, ImageOcr};
use crate::Res;
use std::time::Duration;

/// Configuration and client for a remote OpenAI-compatible OCR endpoint.
pub struct RemoteEndpoint {
    /// Base URL with no trailing slash, e.g. `https://host:8080`. `/v1/chat/completions`
    /// is appended for inference, `/v1/models` for the load-time probe.
    pub base_url: String,
    /// Optional bearer token sent as `Authorization: Bearer <key>`.
    pub api_key: Option<String>,
    /// Optional model name placed in the request body's `"model"` field. Required
    /// by multi-model gateways (litellm, vLLM); a bare remote llama-server ignores
    /// it, so `None` is fine there.
    pub model: Option<String>,
}

impl RemoteEndpoint {
    /// Cheap reachability check used at load time: GET `{base}/v1/models`. Returns
    /// Ok on any HTTP response; the caller treats an Err as "could not reach the
    /// endpoint" (warn, do not hard-fail, since some servers omit /v1/models).
    pub fn probe(&self) -> Res<()> {
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let api_key = self.api_key.clone();
        super::block_on(async move {
            let client = reqwest::Client::new();
            let mut req = client.get(&url).timeout(Duration::from_secs(10));
            if let Some(key) = &api_key {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
            let resp = req.send().await?;
            let _ = resp.error_for_status()?;
            Ok(())
        })
    }
}

impl ImageOcr for RemoteEndpoint {
    fn ocr_image(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
    ) -> Res<String> {
        ocr_via(
            self.base_url.trim_end_matches('/'),
            self.api_key.as_deref(),
            self.model.as_deref(),
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
            dry_multiplier,
            dry_base,
        )
    }

    fn ocr_image_stream(
        &self,
        prompt: &str,
        data_uri: &str,
        max_tokens: u32,
        repeat_penalty: Option<f32>,
        dry_multiplier: Option<f32>,
        dry_base: Option<f32>,
        on_token: &mut dyn FnMut(&str) -> bool,
        should_cancel: &dyn Fn() -> bool,
    ) -> Res<String> {
        ocr_via_stream(
            self.base_url.trim_end_matches('/'),
            self.api_key.as_deref(),
            self.model.as_deref(),
            prompt,
            data_uri,
            max_tokens,
            repeat_penalty,
            dry_multiplier,
            dry_base,
            on_token,
            should_cancel,
        )
    }
}
