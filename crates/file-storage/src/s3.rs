//! The content of files in an S3-compatible store: RustFS (D5, D40).

use crate::ObjectStore;
use async_trait::async_trait;
use control_plane_common::{Error, Result};
use s3::creds::Credentials;
use s3::{Bucket, BucketConfiguration, Region};

/// Where the store is and how to sign in. No `Debug`: it holds the keys.
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
}

pub struct S3Store {
    bucket: Box<Bucket>,
}

impl S3Store {
    /// Connects, and makes the bucket, private, if it is not there yet.
    pub async fn connect(config: S3Config) -> Result<Self> {
        // RustFS ignores the region, but the signature needs one.
        let region = Region::Custom {
            region: "us-east-1".to_owned(),
            endpoint: config.endpoint,
        };
        let credentials = Credentials::new(
            Some(&config.access_key),
            Some(&config.secret_key),
            None,
            None,
            None,
        )
        .map_err(|e| Error::Internal(format!("S3 credentials: {e}")))?;
        let bucket = Bucket::new(&config.bucket, region.clone(), credentials.clone())
            .map_err(s3)?
            .with_path_style();
        if !bucket.exists().await.map_err(s3)? {
            Bucket::create_with_path_style(
                &config.bucket,
                region,
                credentials,
                BucketConfiguration::private(),
            )
            .await
            .map_err(s3)?;
        }
        Ok(Self { bucket })
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    async fn put(&self, key: &str, content: &[u8], content_type: &str) -> Result<()> {
        self.bucket
            .put_object_with_content_type(key, content, content_type)
            .await
            .map_err(s3)?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let response = self.bucket.get_object(key).await.map_err(s3)?;
        Ok(response.to_vec())
    }
}

fn s3(error: s3::error::S3Error) -> Error {
    Error::Internal(format!("S3: {error}"))
}
