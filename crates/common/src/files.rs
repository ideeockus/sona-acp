//! The contract of File Storage (D5, D56, D106).

use crate::{Alias, Caller, FileId, Result, Timestamp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The most one file may weigh.
pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;

/// A stored file, without its content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: FileId,
    /// A name for people and for saving, never a path into storage.
    pub name: String,
    pub size: u64,
    pub content_type: String,
    /// Who uploaded it: its space is theirs (D56).
    pub owner: Alias,
    pub created_at: Timestamp,
}

/// A file attached to a message: what the reader needs to know before fetching it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: FileId,
    pub name: String,
    pub size: u64,
}

impl From<FileInfo> for Attachment {
    fn from(file: FileInfo) -> Self {
        Self {
            id: file.id,
            name: file.name,
            size: file.size,
        }
    }
}

/// Files of agents and the operator, kept in S3 (D106). A file is read by its owner,
/// by those it was shared with, and by the operator.
#[async_trait]
pub trait FileStorage: Send + Sync {
    /// Stores the content in the caller's space. Refused when it carries an active
    /// Run token (D24).
    async fn upload(
        &self,
        caller: &Caller,
        name: &str,
        content_type: &str,
        content: Vec<u8>,
    ) -> Result<FileInfo>;

    async fn info(&self, caller: &Caller, file: FileId) -> Result<FileInfo>;

    async fn download(&self, caller: &Caller, file: FileId) -> Result<(FileInfo, Vec<u8>)>;

    /// What the caller may read, the newest first, at most two hundred.
    async fn list(&self, caller: &Caller) -> Result<Vec<FileInfo>>;

    /// Lets `with` read the files, as when they are attached to a message to them.
    /// The caller must be able to read every one.
    async fn share(&self, caller: &Caller, files: &[FileId], with: &Alias)
    -> Result<Vec<FileInfo>>;
}
