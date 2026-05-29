use crate::config::IndexeddbConfig;
use indexed_db::{Database, Factory};
use js_sys::JsString;
use js_sys::wasm_bindgen::JsValue;
use opendal::Configurator;
use opendal::raw::oio;
use opendal::raw::{
    Access, AccessorInfo, OpCreateDir, OpDelete, OpList, OpRead, OpStat, OpWrite, RpCreateDir,
    RpDelete, RpList, RpRead, RpStat, RpWrite, build_abs_path, build_rel_path, normalize_root,
};
use opendal::{Buffer, Builder, Capability, EntryMode, ErrorKind, Metadata};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

impl Configurator for IndexeddbConfig {
    type Builder = IndexeddbBuilder;
    fn into_builder(self) -> Self::Builder {
        IndexeddbBuilder { config: self }
    }
}

#[derive(Debug, Default)]
pub struct IndexeddbBuilder {
    config: IndexeddbConfig,
}

const SCHEME: &str = "indexeddb";

impl Builder for IndexeddbBuilder {
    type Config = IndexeddbConfig;

    fn build(self) -> opendal::Result<impl Access> {
        let core = IndexeddbCore {
            db_name: self.config.db_name.unwrap_or_else(|| "opendal".to_string()),
            object_store_name: self
                .config
                .object_store_name
                .unwrap_or_else(|| "main".to_string()),
        };
        let root = normalize_root(self.config.root.as_deref().unwrap_or("/"));

        Ok(IndexeddbBackend::new(core).with_normalized_root(root))
    }
}

#[derive(Debug, Clone)]
pub struct IndexeddbCore {
    db_name: String,
    object_store_name: String,
}

impl IndexeddbCore {
    async fn get_client(&self) -> opendal::Result<Database<opendal::Error>> {
        let factory = Factory::<opendal::Error>::get()
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
        let new_version = {
            if let Ok(db) = factory.open_latest_version(&self.db_name).await {
                if db.object_store_names().contains(&self.object_store_name) {
                    return Ok(db);
                } else {
                    let nv = db.version() + 1;
                    db.close();
                    nv
                }
            } else {
                1
            }
        };

        let db = factory
            .open(&self.db_name, new_version, {
                let object_store_name = self.object_store_name.to_string();
                move |evt| async move {
                    if evt.new_version() == new_version {
                        let db = evt.database();
                        db.build_object_store(&object_store_name)
                            .key_path("k")
                            .create()?;
                    }
                    Ok(())
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
        Ok(db)
    }

    async fn get(&self, path: &str) -> opendal::Result<Option<Buffer>> {
        let db = self.get_client().await?;
        let kv = db
            .transaction(&[&self.object_store_name])
            .run({
                let object_store_name = self.object_store_name.to_string();
                let path = path.to_string();
                move |txn| async move {
                    let store = txn.object_store(object_store_name.as_str())?;
                    let kv = store.get(&JsString::from(path)).await?;
                    Ok(kv)
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?
            .and_then(|kv| serde_wasm_bindgen::from_value::<KeyAndValue>(kv).ok());

        Ok(kv.map(|kv| Buffer::from(kv.v)))
    }

    async fn set(&self, path: &str, value: Buffer) -> opendal::Result<()> {
        let db = self.get_client().await?;
        db.transaction(&[&self.object_store_name])
            .rw()
            .run({
                let object_store_name = self.object_store_name.to_string();
                let path = path.to_string();
                let value = value.to_vec();
                let kv = serde_wasm_bindgen::to_value(&KeyAndValue { k: path, v: value })
                    .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
                move |txn| async move {
                    let store = txn.object_store(object_store_name.as_str())?;
                    store.put(&kv).await?;
                    Ok(())
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;

        Ok(())
    }

    async fn delete(&self, path: &str) -> opendal::Result<()> {
        let db = self.get_client().await?;
        db.transaction(&[&self.object_store_name])
            .rw()
            .run({
                let object_store_name = self.object_store_name.to_string();
                let path = path.to_string();
                move |txn| async move {
                    let store = txn.object_store(object_store_name.as_str())?;
                    store.delete(&JsString::from(path.as_str())).await?;
                    Ok(())
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;

        Ok(())
    }

    async fn scan(&self, path: &str) -> opendal::Result<Vec<String>> {
        let db = self.get_client().await?;

        let keys = db
            .transaction(&[&self.object_store_name])
            .run({
                let object_store_name = self.object_store_name.to_string();
                let path = path.to_string();
                move |txn| async move {
                    let store = txn.object_store(object_store_name.as_str())?;
                    let mut cursor = store
                        .cursor()
                        .range(JsValue::from_str(&path)..)?
                        .open_key()
                        .await?;
                    let mut keys = Vec::new();

                    while let Some(key) = cursor.key() {
                        if let Some(key) = key.as_string() {
                            if !key.starts_with(&path) {
                                break;
                            }
                            keys.push(key);
                        }

                        cursor.advance(1).await?;
                    }

                    Ok(keys)
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?
            .into_iter()
            .collect::<Vec<_>>();

        Ok(keys)
    }
}

#[derive(Serialize, Deserialize)]
struct KeyAndValue {
    k: String,
    #[serde(with = "serde_bytes")]
    v: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct IndexeddbBackend {
    core: Arc<IndexeddbCore>,
    root: String,
    info: Arc<AccessorInfo>,
}

impl IndexeddbBackend {
    fn new(core: IndexeddbCore) -> Self {
        let info = AccessorInfo::default();
        info.set_scheme(SCHEME);
        info.set_name(format!("{}-{}", core.db_name, core.object_store_name).as_str());
        info.set_root("/");
        info.set_native_capability(Capability {
            read: true,
            write: true,
            write_can_empty: true,
            create_dir: true,
            delete: true,
            stat: true,
            list: true,
            list_with_recursive: true,
            shared: false,
            ..Default::default()
        });

        Self {
            core: Arc::new(core),
            root: "/".to_string(),
            info: Arc::new(info),
        }
    }

    fn with_normalized_root(mut self, root: String) -> Self {
        self.info.set_root(&root);
        self.root = root;
        self
    }
}

impl Access for IndexeddbBackend {
    type Reader = Buffer;
    type Writer = IndexeddbWriter;
    type Lister = oio::HierarchyLister<IndexeddbLister>;
    type Deleter = oio::OneShotDeleter<IndexeddbDeleter>;

    fn info(&self) -> Arc<AccessorInfo> {
        self.info.clone()
    }

    async fn create_dir(&self, path: &str, _: OpCreateDir) -> opendal::Result<RpCreateDir> {
        let p = build_abs_path(&self.root, path);

        if p != build_abs_path(&self.root, "") {
            self.core.set(&p, Buffer::new()).await?;
        }

        Ok(RpCreateDir::default())
    }

    async fn stat(&self, path: &str, _: OpStat) -> opendal::Result<RpStat> {
        let p = build_abs_path(&self.root, path);

        if p == build_abs_path(&self.root, "") {
            Ok(RpStat::new(Metadata::new(EntryMode::DIR)))
        } else {
            match self.core.get(&p).await? {
                Some(bs) => {
                    let mode = EntryMode::from_path(path);
                    let mut meta = Metadata::new(mode);
                    if mode.is_file() {
                        meta.set_content_length(bs.len() as u64);
                    }
                    Ok(RpStat::new(meta))
                }
                None => Err(opendal::Error::new(
                    ErrorKind::NotFound,
                    "indexeddb doesn't have this path",
                )),
            }
        }
    }

    async fn read(&self, path: &str, args: OpRead) -> opendal::Result<(RpRead, Self::Reader)> {
        let p = build_abs_path(&self.root, path);
        let bs = match self.core.get(&p).await? {
            Some(bs) => bs,
            None => {
                return Err(opendal::Error::new(
                    ErrorKind::NotFound,
                    "indexeddb doesn't have this path",
                ));
            }
        };

        Ok((RpRead::new(), bs.slice(args.range().to_range_as_usize())))
    }

    async fn write(&self, path: &str, _: OpWrite) -> opendal::Result<(RpWrite, Self::Writer)> {
        let p = build_abs_path(&self.root, path);
        Ok((RpWrite::new(), IndexeddbWriter::new(self.core.clone(), p)))
    }

    async fn delete(&self) -> opendal::Result<(RpDelete, Self::Deleter)> {
        Ok((
            RpDelete::default(),
            oio::OneShotDeleter::new(IndexeddbDeleter::new(self.core.clone(), self.root.clone())),
        ))
    }

    async fn list(&self, path: &str, args: OpList) -> opendal::Result<(RpList, Self::Lister)> {
        let p = build_abs_path(&self.root, path);
        let keys = self
            .core
            .scan(&p)
            .await?
            .into_iter()
            .filter(|key| key != &p)
            .collect();
        let lister = IndexeddbLister::new(&self.root, keys);
        let lister = oio::HierarchyLister::new(lister, path, args.recursive());

        Ok((RpList::default(), lister))
    }
}

pub struct IndexeddbLister {
    root: String,
    keys: <Vec<String> as IntoIterator>::IntoIter,
}

impl IndexeddbLister {
    fn new(root: &str, keys: Vec<String>) -> Self {
        Self {
            root: root.to_string(),
            keys: keys.into_iter(),
        }
    }
}

impl oio::List for IndexeddbLister {
    async fn next(&mut self) -> opendal::Result<Option<oio::Entry>> {
        match self.keys.next() {
            Some(key) => {
                let mut path = build_rel_path(&self.root, &key);
                if path.is_empty() {
                    path = "/".to_string();
                }
                let mode = EntryMode::from_path(&path);
                Ok(Some(oio::Entry::new(&path, Metadata::new(mode))))
            }
            None => Ok(None),
        }
    }
}

pub struct IndexeddbWriter {
    core: Arc<IndexeddbCore>,
    path: String,
    buffer: oio::QueueBuf,
}

impl IndexeddbWriter {
    fn new(core: Arc<IndexeddbCore>, path: String) -> Self {
        Self {
            core,
            path,
            buffer: oio::QueueBuf::new(),
        }
    }
}

impl oio::Write for IndexeddbWriter {
    async fn write(&mut self, bs: Buffer) -> opendal::Result<()> {
        self.buffer.push(bs);
        Ok(())
    }

    async fn close(&mut self) -> opendal::Result<Metadata> {
        let buf = self.buffer.clone().collect();
        let length = buf.len() as u64;
        self.core.set(&self.path, buf).await?;

        Ok(Metadata::new(EntryMode::from_path(&self.path)).with_content_length(length))
    }

    async fn abort(&mut self) -> opendal::Result<()> {
        self.buffer.clear();
        Ok(())
    }
}

pub struct IndexeddbDeleter {
    core: Arc<IndexeddbCore>,
    root: String,
}

impl IndexeddbDeleter {
    fn new(core: Arc<IndexeddbCore>, root: String) -> Self {
        Self { core, root }
    }
}

impl oio::OneShotDelete for IndexeddbDeleter {
    async fn delete_once(&self, path: String, _: OpDelete) -> opendal::Result<()> {
        let p = build_abs_path(&self.root, &path);
        self.core.delete(&p).await?;
        Ok(())
    }
}
