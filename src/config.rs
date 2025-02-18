use serde::{Deserialize, Serialize};
#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Hash, Clone)]
#[serde(default)]
pub struct IndexeddbConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_store_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}
