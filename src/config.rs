use serde::{Deserialize, Serialize};

/// Configuration for the IndexedDB OpenDAL service.
#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Hash, Clone)]
#[serde(default)]
pub struct IndexeddbConfig {
    /// IndexedDB database name.
    ///
    /// Defaults to `opendal`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_name: Option<String>,

    /// IndexedDB object store name for object data.
    ///
    /// Defaults to `main`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_store_name: Option<String>,

    /// Root path prefix inside the object store.
    ///
    /// Defaults to `/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}
