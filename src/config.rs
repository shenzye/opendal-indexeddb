use serde::{Deserialize, Serialize};
#[derive(Default, Debug, Serialize, Deserialize, PartialEq, Hash, Clone)]
#[serde(default)]
pub struct IndexeddbConfig {
    pub db_name: Option<String>,
    pub object_store_name: Option<String>,
    pub root: Option<String>,
}
