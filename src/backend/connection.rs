use futures_util::lock::Mutex;
use gloo_timers::future::TimeoutFuture;
use indexed_db::Database;
use js_sys::JsString;
use js_sys::wasm_bindgen::JsValue;
use opendal::ErrorKind;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use wasm_bindgen_futures::spawn_local;

const CONNECTION_CACHE_TTL_MS: f64 = 500.0;

thread_local! {
    static UPGRADE_LOCKS: RefCell<HashMap<String, Arc<Mutex<()>>>> = RefCell::new(HashMap::new());
    static CONNECTION_CACHE_GENERATION: RefCell<u64> = const { RefCell::new(0) };
    pub(super) static CONNECTION_CACHE: RefCell<HashMap<String, CachedConnection>> = RefCell::new(HashMap::new());
}

pub(super) struct CachedConnection {
    pub(super) db: Database<opendal::Error>,
    pub(super) object_store_names: Vec<String>,
    pub(super) last_used_ms: f64,
    pub(super) generation: u64,
}

pub(super) fn upgrade_lock(db_name: &str) -> Arc<Mutex<()>> {
    UPGRADE_LOCKS.with(|locks| {
        let mut locks = locks.borrow_mut();
        locks
            .entry(db_name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    })
}

pub(super) fn cache_connection(
    db_name: &str,
    db: Database<opendal::Error>,
    object_store_names: Vec<String>,
) {
    let generation = next_connection_generation();
    CONNECTION_CACHE.with(|cache| {
        let old = cache.borrow_mut().insert(
            db_name.to_string(),
            CachedConnection {
                db,
                object_store_names,
                last_used_ms: js_sys::Date::now(),
                generation,
            },
        );
        if let Some(old) = old {
            old.db.close();
        }
    });
    spawn_cache_cleanup(db_name.to_string(), generation);
}

pub(super) fn evict_cached_connection(db_name: &str) {
    CONNECTION_CACHE.with(|cache| {
        if let Some(cached) = cache.borrow_mut().remove(db_name) {
            cached.db.close();
        }
    });
}

pub(super) fn is_stale_connection_error(err: &indexed_db::Error<opendal::Error>) -> bool {
    matches!(
        err,
        indexed_db::Error::DatabaseIsClosed
            | indexed_db::Error::DoesNotExist
            | indexed_db::Error::ObjectStoreWasRemoved
    )
}

pub(super) fn cached_connection_expired(cached: &CachedConnection) -> bool {
    js_sys::Date::now() - cached.last_used_ms > CONNECTION_CACHE_TTL_MS
}

fn next_connection_generation() -> u64 {
    CONNECTION_CACHE_GENERATION.with(|counter| {
        let mut counter = counter.borrow_mut();
        *counter = counter.wrapping_add(1);
        *counter
    })
}

fn spawn_cache_cleanup(db_name: String, generation: u64) {
    spawn_local(async move {
        while let Some(sleep_ms) = cached_connection_cleanup_delay_ms(&db_name, generation) {
            if sleep_ms > 0 {
                TimeoutFuture::new(sleep_ms).await;
            }

            let should_keep_waiting = CONNECTION_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                let Some(cached) = cache.get(&db_name) else {
                    return false;
                };
                if cached.generation != generation {
                    return false;
                }
                if cached_connection_expired(cached) {
                    if let Some(cached) = cache.remove(&db_name) {
                        cached.db.close();
                    }
                    return false;
                }
                true
            });
            if !should_keep_waiting {
                break;
            }
        }
    });
}

fn cached_connection_cleanup_delay_ms(db_name: &str, generation: u64) -> Option<u32> {
    CONNECTION_CACHE.with(|cache| {
        let cache = cache.borrow();
        let cached = cache.get(db_name)?;
        if cached.generation != generation {
            return None;
        }
        if cached_connection_expired(cached) {
            return Some(0);
        }
        let deadline_ms = cached.last_used_ms + CONNECTION_CACHE_TTL_MS;
        let remaining_ms = (deadline_ms - js_sys::Date::now()).ceil().max(1.0);
        Some(remaining_ms.min(u32::MAX as f64) as u32)
    })
}

pub(super) struct RecursiveDeleteTarget {
    pub(super) exact_key: Option<String>,
    pub(super) prefix: String,
}

pub(super) fn recursive_delete_target(path: String) -> RecursiveDeleteTarget {
    if path.is_empty() {
        RecursiveDeleteTarget {
            exact_key: None,
            prefix: String::new(),
        }
    } else if path.ends_with('/') {
        RecursiveDeleteTarget {
            exact_key: None,
            prefix: path,
        }
    } else {
        RecursiveDeleteTarget {
            prefix: format!("{path}/"),
            exact_key: Some(path),
        }
    }
}

pub(super) fn prefix_upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    while let Some(byte) = bytes.pop() {
        if byte != u8::MAX {
            bytes.push(byte + 1);
            // `prefix` is valid UTF-8. Truncating at a byte that can be
            // incremented preserves UTF-8 for this backend's path strings.
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

pub(super) async fn delete_prefix(
    store: &indexed_db::ObjectStore<opendal::Error>,
    meta_store: &indexed_db::ObjectStore<opendal::Error>,
    prefix: &str,
) -> Result<(), indexed_db::Error<opendal::Error>> {
    if prefix.is_empty() {
        store.clear().await?;
        meta_store.clear().await?;
        return Ok(());
    }

    if let Some(upper) = prefix_upper_bound(prefix) {
        store
            .delete_range(JsValue::from_str(prefix)..JsValue::from_str(&upper))
            .await?;
        meta_store
            .delete_range(JsValue::from_str(prefix)..JsValue::from_str(&upper))
            .await?;
        return Ok(());
    }

    delete_prefix_by_cursor(store, meta_store, prefix).await
}

async fn delete_prefix_by_cursor(
    store: &indexed_db::ObjectStore<opendal::Error>,
    meta_store: &indexed_db::ObjectStore<opendal::Error>,
    prefix: &str,
) -> Result<(), indexed_db::Error<opendal::Error>> {
    let mut cursor = store
        .cursor()
        .range(JsValue::from_str(prefix)..)?
        .open_key()
        .await?;
    while let Some(key) = cursor.key() {
        if let Some(key) = key.as_string() {
            if !key.starts_with(prefix) {
                break;
            }
            let key = JsString::from(key.as_str());
            store.delete(&key).await?;
            meta_store.delete(&key).await?;
        }
        cursor.advance(1).await?;
    }
    Ok(())
}

/// Recovers an opendal error raised inside a transaction closure; anything
/// else is an infrastructure failure reported by indexed_db itself.
pub(super) fn parse_txn_error(err: indexed_db::Error<opendal::Error>) -> opendal::Error {
    match err {
        indexed_db::Error::User(err) => err,
        err => opendal::Error::new(ErrorKind::Unexpected, err.to_string()),
    }
}
