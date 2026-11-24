//! Private retrieval representation. The FFI owns the wire projection.

use respire_protocol::MemoryMeta;

#[derive(Debug, Clone)]
pub struct StoredMemory {
    pub id: String,         // uuid v4 (client-generated, no multi-device clash)
    pub updated_at: String, // RFC3339 milliseconds
    pub deleted: bool,      // tombstone

    // Materialized request-local metadata and features; not serialized.
    pub local_meta: MemoryMeta,
    pub local_kind: String,
    pub local_tags: String,
    pub local_title: String,
    pub local_project: String,
    pub local_computer: String,
    pub local_embedding: Option<Vec<f32>>,
    pub local_parent_id: String,
    pub local_created_at: String,
    /// Local plaintext content head (BM25-style lexical search).
    pub local_content_head: String,
    /// Local hit count (recency/heat axis).
    pub local_recall_count: i64,
    /// Importance (request-local projection).
    pub local_importance: String,
    /// Chunk-level embeddings (request-local derived data).
    ///
    /// A long entry collapses into one vector and its tail is diluted away. Measured on LoCoMo,
    /// chunked indexing lifts session hit from 0.5228 to 0.5990 on the Chinese model and from
    /// 0.6497 to 0.8680 on bge-m3. Retrieval scores the whole entry **and** every chunk, then keeps
    /// the max; what gets returned is still the whole entry — index fine, hand back coarse.
    pub local_chunks: Vec<Vec<f32>>,
}

impl StoredMemory {
    pub fn new_pending(id: String) -> Self {
        Self {
            id,
            updated_at: String::new(),
            deleted: false,
            local_meta: MemoryMeta::default(),
            local_kind: String::new(),
            local_tags: String::new(),
            local_title: String::new(),
            local_project: String::new(),
            local_computer: String::new(),
            local_embedding: None,
            local_parent_id: String::new(),
            local_created_at: String::new(),
            local_content_head: String::new(),
            local_recall_count: 0,
            local_importance: "normal".to_owned(),
            local_chunks: Vec::new(),
        }
    }
}
