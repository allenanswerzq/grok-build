//! S3 presigning for builds without the `direct-upload` feature: every call fails.

pub struct S3StaticCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

fn unavailable() -> anyhow::Error {
    anyhow::anyhow!("S3 is not built into this binary")
}

pub async fn presign_put_url(
    _region: &str,
    _endpoint_url: Option<&str>,
    _creds: &S3StaticCredentials,
    _bucket: &str,
    _key: &str,
    _content_type: &str,
    _expires_in: std::time::Duration,
) -> anyhow::Result<String> {
    Err(unavailable())
}

pub async fn presign_get_url(
    _region: &str,
    _endpoint_url: Option<&str>,
    _creds: &S3StaticCredentials,
    _bucket: &str,
    _key: &str,
    _expires_in: std::time::Duration,
) -> anyhow::Result<String> {
    Err(unavailable())
}
