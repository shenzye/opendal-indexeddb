use super::access::IndexeddbBackend;
use super::core::IndexeddbCore;
use crate::config::IndexeddbConfig;
use opendal::raw::{Access, normalize_root};
use opendal::{Builder, Configurator, ErrorKind};

const META_STORE_PREFIX: &str = "__opendal_indexeddb_meta__";

impl Configurator for IndexeddbConfig {
    type Builder = IndexeddbBuilder;
    fn into_builder(self) -> Self::Builder {
        IndexeddbBuilder { config: self }
    }
}

#[derive(Debug, Default)]
/// Builder for the IndexedDB OpenDAL service.
///
/// The service is available only on wasm targets and stores objects in a
/// browser IndexedDB database.
pub struct IndexeddbBuilder {
    config: IndexeddbConfig,
}

impl IndexeddbBuilder {
    /// Set the IndexedDB database name. Defaults to `opendal`.
    pub fn db_name(mut self, db_name: &str) -> Self {
        self.config.db_name = Some(db_name.to_string());
        self
    }

    /// Set the IndexedDB object store name. Defaults to `main`.
    pub fn object_store_name(mut self, object_store_name: &str) -> Self {
        self.config.object_store_name = Some(object_store_name.to_string());
        self
    }

    /// Set the root path inside the object store. Defaults to `/`.
    pub fn root(mut self, root: &str) -> Self {
        self.config.root = Some(root.to_string());
        self
    }
}

impl Builder for IndexeddbBuilder {
    type Config = IndexeddbConfig;

    fn build(self) -> opendal::Result<impl Access> {
        let db_name = match self.config.db_name {
            Some(db_name) if db_name.is_empty() => {
                return Err(opendal::Error::new(
                    ErrorKind::ConfigInvalid,
                    "indexeddb db_name must not be empty",
                )
                .with_context("db_name", db_name));
            }
            Some(db_name) => db_name,
            None => "opendal".to_string(),
        };
        let object_store_name = match self.config.object_store_name {
            Some(object_store_name) if object_store_name.is_empty() => {
                return Err(opendal::Error::new(
                    ErrorKind::ConfigInvalid,
                    "indexeddb object_store_name must not be empty",
                )
                .with_context("object_store_name", object_store_name));
            }
            Some(object_store_name) => object_store_name,
            None => "main".to_string(),
        };
        if object_store_name.starts_with(META_STORE_PREFIX) {
            return Err(opendal::Error::new(
                ErrorKind::ConfigInvalid,
                "indexeddb object_store_name must not start with reserved prefix `__opendal_indexeddb_meta__`",
            )
            .with_context("object_store_name", object_store_name));
        }
        let core = IndexeddbCore {
            db_name,
            meta_store_name: meta_store_name_for(&object_store_name),
            object_store_name,
        };
        let root = normalize_root(self.config.root.as_deref().unwrap_or("/"));

        Ok(IndexeddbBackend::new(core).with_normalized_root(root))
    }
}

fn meta_store_name_for(object_store_name: &str) -> String {
    format!("{META_STORE_PREFIX}{object_store_name}")
}
