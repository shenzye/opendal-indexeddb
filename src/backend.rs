use crate::config::IndexeddbConfig;
use async_once_cell::OnceCell;
use futures::{Stream, StreamExt};
use indexed_db::{Database, Factory};
use js_sys::wasm_bindgen::JsValue;
use js_sys::JsString;
use opendal::raw::adapters::kv;
use opendal::raw::adapters::kv::Info;
use opendal::raw::Access;
use opendal::Configurator;
use opendal::{Buffer, Builder, Capability, ErrorKind, Scheme};
use send_wrapper::SendWrapper;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::task::{Context, Poll};

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

const SCHEME: Scheme = Scheme::Custom("indexeddb");

impl Builder for IndexeddbBuilder {
    const SCHEME: Scheme = SCHEME;
    type Config = IndexeddbConfig;

    fn build(self) -> opendal::Result<impl Access> {
        Ok(IndexeddbBackend::new(Adapter {
            db: OnceCell::new(),
            db_name: self.config.db_name.unwrap_or_else(|| "opendal".to_string()),
            object_store_name: self
                .config
                .object_store_name
                .unwrap_or_else(|| "main".to_string()),
        })
        .with_root(
            self.config
                .root
                .as_ref()
                .map(String::as_str)
                .unwrap_or_else(|| "/"),
        ))
    }
}

#[derive(Debug)]
pub struct Adapter {
    db: OnceCell<SendWrapper<Database<opendal::Error>>>,
    db_name: String,
    object_store_name: String,
}

impl Adapter {
    async fn get_client(&self) -> opendal::Result<&SendWrapper<Database<opendal::Error>>> {
        self.db
            .get_or_try_init(async {
                let factory = Factory::<opendal::Error>::get()
                    .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
                let db = factory
                    .open(self.db_name.as_str(), 1, {
                        let object_store_name = self.object_store_name.to_string();
                        move |evt| async move {
                            if evt.new_version() == 1 {
                                let db = evt.database();
                                db.build_object_store(object_store_name.as_str())
                                    .key_path("k")
                                    .create()?;
                            }
                            Ok(())
                        }
                    })
                    .await
                    .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
                Ok(SendWrapper::new(db))
            })
            .await
    }
}

#[derive(Serialize, Deserialize)]
struct KeyAndValue {
    k: String,
    #[serde(with = "serde_bytes")]
    v: Vec<u8>,
}

pub struct IndexeddbScanner {
    keys: <Vec<String> as IntoIterator>::IntoIter,
}

impl Stream for IndexeddbScanner {
    type Item = opendal::Result<String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.keys.next().map(|k| Ok(k)))
    }
}

impl kv::Scan for IndexeddbScanner {
    async fn next(&mut self) -> opendal::Result<Option<String>> {
        <Self as StreamExt>::next(self).await.transpose()
    }
}

impl kv::Adapter for Adapter {
    type Scanner = IndexeddbScanner;

    fn info(&self) -> Info {
        Info::new(
            SCHEME,
            format!("{}-{}", self.db_name, self.object_store_name).as_str(),
            Capability {
                read: true,
                write: true,
                delete: true,
                list: true,
                shared: false,
                ..Default::default()
            },
        )
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
            .map(|kv| serde_wasm_bindgen::from_value::<KeyAndValue>(kv).ok())
            .flatten();

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

    async fn scan(&self, path: &str) -> opendal::Result<Self::Scanner> {
        let db = self.get_client().await?;

        let keys = db
            .transaction(&[&self.object_store_name])
            .run({
                let object_store_name = self.object_store_name.to_string();
                let path = path.to_string();
                move |txn| async move {
                    let store = txn.object_store(object_store_name.as_str())?;
                    let kv = store.get_all_keys_in(JsValue::from(path).., None).await?;
                    Ok(kv)
                }
            })
            .await
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?
            .into_iter()
            .filter_map(|k| k.as_string())
            .collect::<Vec<_>>();

        Ok(Self::Scanner {
            keys: keys.into_iter(),
        })
    }
}

pub type IndexeddbBackend = kv::Backend<Adapter>;
