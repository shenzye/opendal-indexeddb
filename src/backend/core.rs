use super::connection::{
    CONNECTION_CACHE, cache_connection, cached_connection_expired, delete_prefix,
    evict_cached_connection, is_stale_connection_error, parse_txn_error, prefix_upper_bound,
    recursive_delete_target, upgrade_lock,
};
use super::meta::{
    EntryInfo, EntryMeta, KeyAndValue, MetaContent, MetaRecord, ReadArgs, ScanPage,
    ScanPageRequest, WriteArgs, check_etag, check_modified, entry_info_for_key,
    entry_meta_with_etag_fallback, entry_metadata, etag_from_bytes, put_meta, read_meta,
    record_bytes, record_bytes_range, set_record_key, source_size, value_byte_length, value_etag,
};
use futures_util::{
    future::{Either, select},
    pin_mut,
};
use gloo_timers::future::TimeoutFuture;
use indexed_db::{Database, Factory, Transaction};
use js_sys::JsString;
use js_sys::wasm_bindgen::JsValue;
use opendal::{Buffer, ErrorKind, Metadata};
use std::collections::HashMap;
use std::future::Future;
use std::ops::Bound;
use std::sync::Arc;

const META_JOIN_MIN_SIZE: usize = 32;
const OPEN_TIMEOUT_MS: u32 = 10_000;

/// Returns string keys and the raw IndexedDB key count fetched for a range.
///
/// This backend only writes string keys. Non-string keys can only be introduced
/// by external code mutating the same object store. Such keys are skipped; if a
/// whole fetched page contains no string keys, callers must stop at that point
/// because we cannot safely advance the string lower bound from arbitrary
/// IndexedDB key types without depending on cross-type key ordering.
pub(crate) async fn get_all_string_keys_in(
    store: &indexed_db::ObjectStore<opendal::Error>,
    lower: &str,
    lower_open: bool,
    upper: Option<&str>,
    limit: usize,
) -> Result<(Vec<String>, usize), indexed_db::Error<opendal::Error>> {
    let lower = JsValue::from_str(lower);
    let lower = if lower_open {
        Bound::Excluded(lower)
    } else {
        Bound::Included(lower)
    };

    let keys = match upper {
        Some(upper) => {
            let upper = JsValue::from_str(upper);
            store
                .get_all_keys_in((lower, Bound::Excluded(upper)), Some(limit as u32))
                .await?
        }
        None => {
            store
                .get_all_keys_in((lower, Bound::Unbounded), Some(limit as u32))
                .await?
        }
    };
    let raw_len = keys.len();

    Ok((
        keys.into_iter().filter_map(|key| key.as_string()).collect(),
        raw_len,
    ))
}

#[derive(Debug, Clone)]
pub struct IndexeddbCore {
    pub(crate) db_name: String,
    pub(crate) object_store_name: String,
    pub(crate) meta_store_name: String,
}

impl IndexeddbCore {
    /// Opens a connection to the database, upgrading it if our stores are
    /// missing. The caller must hold this database's upgrade lock so no other
    /// task can open and cache a connection while an upgrade is being prepared.
    async fn open_client(&self) -> opendal::Result<Database<opendal::Error>> {
        let factory = Factory::<opendal::Error>::get()
            .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;

        if let Some(db) = self.open_if_stores_exist(&factory).await {
            return Ok(db);
        }

        // Only one operator in this JS realm upgrades a database at a time.
        // Cross-tab upgrades can still win a version race, so keep a bounded
        // retry loop around the locked upgrade path.
        evict_cached_connection(&self.db_name);
        for _ in 0..8 {
            let new_version = {
                if let Ok(db) = factory.open_latest_version(&self.db_name).await {
                    let names = db.object_store_names();
                    if names.contains(&self.object_store_name)
                        && names.contains(&self.meta_store_name)
                    {
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

            let open = factory.open(&self.db_name, new_version, {
                let object_store_name = self.object_store_name.to_string();
                let meta_store_name = self.meta_store_name.to_string();
                move |evt| async move {
                    let db = evt.database();
                    let names = db.object_store_names();
                    if !names.contains(&object_store_name) {
                        db.build_object_store(&object_store_name)
                            .key_path("k")
                            .create()?;
                    }
                    if !names.contains(&meta_store_name) {
                        db.build_object_store(&meta_store_name)
                            .key_path("k")
                            .create()?;
                    }
                    Ok(())
                }
            });
            let timeout = TimeoutFuture::new(OPEN_TIMEOUT_MS);
            pin_mut!(open);
            pin_mut!(timeout);
            let result = match select(open, timeout).await {
                Either::Left((result, _)) => result,
                Either::Right((_, _)) => {
                    return Err(opendal::Error::new(
                        ErrorKind::Unexpected,
                        "indexeddb version upgrade timed out: another connection is blocking the upgrade",
                    )
                    .with_context("db_name", &self.db_name));
                }
            };

            match result {
                Ok(db) => {
                    let names = db.object_store_names();
                    if names.contains(&self.object_store_name)
                        && names.contains(&self.meta_store_name)
                    {
                        return Ok(db);
                    }
                    // The version was already taken by a concurrent upgrade
                    // that didn't create our stores.
                    db.close();
                }
                Err(indexed_db::Error::VersionTooOld) => {}
                Err(err) => {
                    return Err(opendal::Error::new(ErrorKind::Unexpected, err.to_string())
                        .with_context("db_name", &self.db_name));
                }
            }
        }

        Err(opendal::Error::new(
            ErrorKind::Unexpected,
            "failed to open indexeddb database: too many concurrent version upgrades",
        )
        .with_context("db_name", &self.db_name))
    }

    async fn open_if_stores_exist(
        &self,
        factory: &Factory<opendal::Error>,
    ) -> Option<Database<opendal::Error>> {
        let db = factory.open_latest_version(&self.db_name).await.ok()?;
        let names = db.object_store_names();
        if names.contains(&self.object_store_name) && names.contains(&self.meta_store_name) {
            Some(db)
        } else {
            db.close();
            None
        }
    }

    async fn ensure_cached_client(&self) -> opendal::Result<()> {
        if self.cached_client_has_stores() {
            return Ok(());
        }

        let lock = upgrade_lock(&self.db_name);
        let _guard = lock.lock().await;
        if self.cached_client_has_stores() {
            return Ok(());
        }

        evict_cached_connection(&self.db_name);
        let db = self.open_client().await?;
        let object_store_names = db.object_store_names();
        cache_connection(&self.db_name, db, object_store_names);
        Ok(())
    }

    fn cached_client_has_stores(&self) -> bool {
        CONNECTION_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let Some(cached) = cache.get_mut(&self.db_name) else {
                return false;
            };
            if cached_connection_expired(cached) {
                if let Some(cached) = cache.remove(&self.db_name) {
                    cached.db.close();
                }
                return false;
            }
            cached.last_used_ms = js_sys::Date::now();
            self.stores_exist_in(&cached.object_store_names)
        })
    }

    fn stores_exist_in(&self, names: &[String]) -> bool {
        names.contains(&self.object_store_name) && names.contains(&self.meta_store_name)
    }

    fn cached_transaction(
        &self,
        readwrite: bool,
    ) -> Option<indexed_db::TransactionBuilder<opendal::Error>> {
        CONNECTION_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let cached = cache.get_mut(&self.db_name)?;
            if cached_connection_expired(cached) {
                if let Some(cached) = cache.remove(&self.db_name) {
                    cached.db.close();
                }
                return None;
            }
            if !self.stores_exist_in(&cached.object_store_names) {
                return None;
            }
            cached.last_used_ms = js_sys::Date::now();

            let stores = [
                self.object_store_name.as_str(),
                self.meta_store_name.as_str(),
            ];
            let transaction = cached.db.transaction(&stores);
            Some(if readwrite {
                transaction.rw()
            } else {
                transaction
            })
        })
    }

    async fn run_transaction<Make, Fun, Fut, Ret>(
        &self,
        readwrite: bool,
        make: Make,
    ) -> opendal::Result<Ret>
    where
        Make: Fn() -> Fun,
        Fun: 'static + FnOnce(Transaction<opendal::Error>) -> Fut,
        Fut: 'static + Future<Output = Result<Ret, indexed_db::Error<opendal::Error>>>,
        Ret: 'static,
    {
        for attempt in 0..2 {
            self.ensure_cached_client().await?;
            let Some(transaction) = self.cached_transaction(readwrite) else {
                evict_cached_connection(&self.db_name);
                continue;
            };

            match transaction.run(make()).await {
                Ok(value) => return Ok(value),
                Err(err) if attempt == 0 && is_stale_connection_error(&err) => {
                    evict_cached_connection(&self.db_name);
                }
                Err(err) => return Err(parse_txn_error(err)),
            }
        }

        Err(opendal::Error::new(
            ErrorKind::Unexpected,
            "failed to run indexeddb transaction after refreshing cached connection",
        )
        .with_context("db_name", &self.db_name))
    }

    pub(super) async fn read(
        &self,
        path: &str,
        read_args: ReadArgs,
    ) -> opendal::Result<Option<(EntryInfo, Buffer)>> {
        let if_match = read_args.if_match;
        let if_none_match = read_args.if_none_match;
        let if_modified_since = read_args.if_modified_since;
        let if_unmodified_since = read_args.if_unmodified_since;
        let range = read_args.range;
        let has_conditions = if_match.is_some()
            || if_none_match.is_some()
            || if_modified_since.is_some()
            || if_unmodified_since.is_some();

        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        let path = path.to_string();
        self.run_transaction(false, move || {
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let path = path.clone();
            let if_match = if_match.clone();
            let if_none_match = if_none_match.clone();
            let if_modified_since = if_modified_since;
            let if_unmodified_since = if_unmodified_since;
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let key = JsString::from(path.as_str());

                let meta = match read_meta(&meta_store, &key).await? {
                    Some(meta) if meta.e.is_some() => {
                        let info = EntryInfo {
                            meta: EntryMeta::from(meta),
                        };
                        if has_conditions && !store.contains(&key).await? {
                            return Ok(None);
                        }
                        check_modified(&info, if_modified_since, if_unmodified_since)
                            .map_err(|err| err.with_context("path", &path))?;
                        check_etag(&info, if_match.as_deref(), if_none_match.as_deref())
                            .map_err(|err| err.with_context("path", &path))?;

                        let Some(value) = store.get(&key).await? else {
                            return Ok(None);
                        };
                        let total_size = info
                            .meta
                            .size
                            .or_else(|| value_byte_length(&value))
                            .unwrap_or(0);
                        let bytes = record_bytes_range(&value, path.as_str(), range, total_size)?;
                        return Ok(Some((info, Buffer::from(bytes))));
                    }
                    meta => meta,
                };

                let Some(value) = store.get(&key).await? else {
                    return Ok(None);
                };
                let info = match meta {
                    Some(meta) => EntryInfo {
                        meta: entry_meta_with_etag_fallback(meta, value_etag(&value)),
                    },
                    None => EntryInfo {
                        meta: EntryMeta::from_size(value_byte_length(&value))
                            .with_etag(value_etag(&value)),
                    },
                };
                check_modified(&info, if_modified_since, if_unmodified_since)
                    .map_err(|err| err.with_context("path", &path))?;
                check_etag(&info, if_match.as_deref(), if_none_match.as_deref())
                    .map_err(|err| err.with_context("path", &path))?;

                let total_size = info
                    .meta
                    .size
                    .or_else(|| value_byte_length(&value))
                    .unwrap_or(0);
                let bytes = record_bytes_range(&value, path.as_str(), range, total_size)?;
                Ok(Some((info, Buffer::from(bytes))))
            }
        })
        .await
    }

    pub(super) async fn set(
        &self,
        path: &str,
        value: Buffer,
        write_args: WriteArgs,
    ) -> opendal::Result<EntryInfo> {
        let incoming = Arc::new(value.to_vec());
        let if_not_exists = write_args.if_not_exists;
        let if_match = write_args.if_match;
        let if_none_match = write_args.if_none_match;
        let append = write_args.append;
        let request_meta = write_args.meta.clone();
        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        let path = path.to_string();

        self.run_transaction(true, move || {
            let incoming = incoming.clone();
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let path = path.clone();
            let if_match = if_match.clone();
            let if_none_match = if_none_match.clone();
            let request_meta = request_meta.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let key = JsString::from(path.as_str());
                let unconditional_overwrite =
                    !append && !if_not_exists && if_match.is_none() && if_none_match.is_none();

                // Read the existing record once to evaluate every write
                // condition against the same pre-write state. The data
                // store is the source of truth for existence; meta may be an
                // orphan left by an older delete that only touched the data
                // store, so never trust meta alone.
                let existing_data = if unconditional_overwrite {
                    None
                } else {
                    store.get(&key).await?
                };
                let exists = existing_data.is_some();
                let existing_meta = if exists {
                    read_meta(&meta_store, &key).await?
                } else {
                    None
                };
                let existing_etag = existing_meta
                    .as_ref()
                    .and_then(|m| m.e.clone())
                    .or_else(|| existing_data.as_ref().and_then(value_etag));

                if if_not_exists && exists {
                    return Err(opendal::Error::new(
                        ErrorKind::ConditionNotMatch,
                        "path already exists",
                    )
                    .with_context("path", &path)
                    .into());
                }
                if let Some(want) = if_match.as_deref() {
                    let matched = want == "*" && exists || existing_etag.as_deref() == Some(want);
                    if !matched {
                        return Err(opendal::Error::new(
                            ErrorKind::ConditionNotMatch,
                            "the entry's etag does not match if_match",
                        )
                        .with_context("path", &path)
                        .into());
                    }
                }
                if let Some(want) = if_none_match.as_deref() {
                    let matched = want == "*" && exists || existing_etag.as_deref() == Some(want);
                    if matched {
                        return Err(opendal::Error::new(
                            ErrorKind::ConditionNotMatch,
                            "the entry's etag matches if_none_match",
                        )
                        .with_context("path", &path)
                        .into());
                    }
                }

                let mut bytes = if append {
                    match existing_data.as_ref() {
                        Some(value) => record_bytes(value, path.as_str())?,
                        None => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                bytes.extend_from_slice(incoming.as_slice());

                // The ETag is derived from the content, so identical bytes
                // always produce the same ETag (matches S3 single-part copy
                // semantics). For append writes this is the ETag of the
                // post-append object.
                let etag = etag_from_bytes(&bytes);
                let length = bytes.len() as f64;
                let modified_ms = Some(js_sys::Date::now());
                let kv = serde_wasm_bindgen::to_value(&KeyAndValue {
                    k: path.to_string(),
                    v: bytes,
                })
                .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;

                let mut meta_content = match (append, existing_meta.as_ref()) {
                    (true, Some(meta)) => MetaContent::from(meta),
                    _ => MetaContent::default(),
                };
                if let Some(request_meta) = request_meta {
                    meta_content.merge(request_meta);
                }
                meta_content.etag = Some(etag);
                let info = EntryInfo {
                    meta: EntryMeta {
                        size: Some(length as u64),
                        last_modified_ms: modified_ms,
                        etag: meta_content.etag.clone(),
                        content: Some(meta_content.clone()),
                    },
                };
                let meta = serde_wasm_bindgen::to_value(&MetaRecord::new(
                    path.to_string(),
                    length,
                    modified_ms,
                    Some(meta_content),
                ))
                .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;

                store.put(&kv).await?;
                meta_store.put(&meta).await?;
                Ok(info)
            }
        })
        .await
    }

    pub(super) async fn delete(&self, path: &str) -> opendal::Result<()> {
        self.delete_many(vec![(path.to_string(), false)]).await
    }

    /// Removes `path` and every entry nested under it in one transaction.
    ///
    /// IndexedDB stores are ordered by key, so a directory's children form a
    /// contiguous range keyed by the directory prefix. Recursive deletes remove
    /// those key ranges directly from both the data and meta stores.
    pub(super) async fn delete_recursive(&self, path: &str) -> opendal::Result<()> {
        self.delete_many(vec![(path.to_string(), true)]).await
    }

    /// Removes all requested paths in one IndexedDB transaction.
    ///
    /// Recursive entries delete their matching key ranges directly. Exact keys
    /// and non-recursive entries are dropped from both the data and metadata
    /// sidecars. IndexedDB delete is idempotent, so duplicate keys from
    /// overlapping recursive requests are harmless.
    pub(super) async fn delete_many(&self, batch: Vec<(String, bool)>) -> opendal::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let batch = Arc::new(batch);
        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        self.run_transaction(true, move || {
            let batch = batch.clone();
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let mut exact_keys = Vec::new();

                for (path, recursive) in batch.iter() {
                    if !recursive {
                        exact_keys.push(path.clone());
                        continue;
                    }

                    let target = recursive_delete_target(path.clone());
                    if let Some(key) = target.exact_key {
                        exact_keys.push(key);
                    }
                    delete_prefix(&store, &meta_store, &target.prefix).await?;
                }

                for key in exact_keys {
                    let key = JsString::from(key.as_str());
                    store.delete(&key).await?;
                    meta_store.delete(&key).await?;
                }
                Ok(())
            }
        })
        .await
    }

    pub(super) async fn stat(&self, path: &str) -> opendal::Result<Option<EntryInfo>> {
        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        let path = path.to_string();
        self.run_transaction(false, move || {
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let path = path.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let key = JsString::from(path.as_str());

                if let Some(value) = meta_store.get(&key).await?
                    && let Ok(meta) = serde_wasm_bindgen::from_value::<MetaRecord>(value)
                {
                    // Guard against an orphan meta record left behind by an
                    // old version of this crate deleting only the data store.
                    if meta.e.is_some() && store.contains(&key).await? {
                        return Ok(Some(EntryInfo {
                            meta: EntryMeta::from(meta),
                        }));
                    }
                    if meta.e.is_none()
                        && let Some(value) = store.get(&key).await?
                    {
                        return Ok(Some(EntryInfo {
                            meta: entry_meta_with_etag_fallback(meta, value_etag(&value)),
                        }));
                    }
                    return Ok(None);
                }

                // Record written before the meta store existed: measure the
                // value and synthesize an ETag from its bytes so conditional
                // operations still work for legacy entries.
                match store.get(&key).await? {
                    Some(value) => Ok(Some(EntryInfo {
                        meta: EntryMeta::from_size(value_byte_length(&value))
                            .with_etag(value_etag(&value)),
                    })),
                    None => Ok(None),
                }
            }
        })
        .await
    }

    pub(super) async fn scan_page(&self, req: ScanPageRequest<'_>) -> opendal::Result<ScanPage> {
        let path = req.prefix.to_string();
        let start_after = req.start_after.map(str::to_string);
        let skip_prefix = req.skip_prefix.map(str::to_string);
        let limit = req.limit;
        if limit == 0 {
            return Ok(ScanPage {
                entries: Vec::new(),
                next_start_after: start_after,
                skip_prefix,
                done: true,
            });
        }

        let upper = prefix_upper_bound(&path);
        let lower = match &start_after {
            Some(start_after) if start_after.as_str() > path.as_str() => start_after.clone(),
            _ => path.clone(),
        };
        if let Some(upper) = upper.as_ref()
            && lower.as_str() >= upper.as_str()
        {
            return Ok(ScanPage {
                entries: Vec::new(),
                next_start_after: start_after,
                skip_prefix,
                done: true,
            });
        }

        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        self.run_transaction(false, move || {
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let path = path.clone();
            let lower = lower.clone();
            let upper = upper.clone();
            let start_after = start_after.clone();
            let scan_skip_prefix = skip_prefix.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let mut keys = Vec::new();
                let mut next_start_after = start_after.clone();
                let mut done = true;
                let mut lower = lower;
                let mut lower_open = start_after
                    .as_ref()
                    .is_some_and(|start_after| start_after.as_str() >= path.as_str());

                'scan: loop {
                    if let Some(upper) = upper.as_ref()
                        && lower.as_str() >= upper.as_str()
                    {
                        break;
                    }

                    let remaining = limit - keys.len();
                    let (fetched, fetched_len) = get_all_string_keys_in(
                        &store,
                        lower.as_str(),
                        lower_open,
                        upper.as_deref(),
                        remaining,
                    )
                    .await?;
                    if fetched_len == 0 {
                        break;
                    }
                    if fetched.is_empty() {
                        // External non-string keys are outside this backend's
                        // data model. Stop instead of spinning on the same
                        // string lower bound forever.
                        break;
                    }

                    for key in fetched {
                        if !key.starts_with(&path) {
                            break 'scan;
                        }
                        if let Some(start_after) = &start_after
                            && key.as_str() <= start_after.as_str()
                        {
                            next_start_after = Some(key.clone());
                            lower = key;
                            lower_open = true;
                            continue;
                        }
                        if key == path && path.ends_with('/') {
                            next_start_after = Some(key.clone());
                            lower = key;
                            lower_open = true;
                            continue;
                        }
                        if let Some(skip_prefix) = &scan_skip_prefix
                            && key.starts_with(skip_prefix)
                        {
                            next_start_after = Some(key.clone());
                            match prefix_upper_bound(skip_prefix) {
                                Some(next_lower) if next_lower.as_str() > lower.as_str() => {
                                    lower = next_lower;
                                    lower_open = false;
                                }
                                Some(_) | None => break 'scan,
                            }
                            continue 'scan;
                        }
                        next_start_after = Some(key.clone());
                        lower = key.clone();
                        lower_open = true;
                        keys.push(key);
                        if keys.len() >= limit {
                            done = false;
                            break 'scan;
                        }
                    }

                    if fetched_len < remaining {
                        break;
                    }
                }

                let mut metas = HashMap::new();
                if keys.len() >= META_JOIN_MIN_SIZE
                    && let (Some(first_key), Some(last_key)) = (keys.first(), keys.last())
                {
                    let mut cursor = meta_store
                        .cursor()
                        .range(JsValue::from_str(first_key)..=JsValue::from_str(last_key))?
                        .open()
                        .await?;
                    while let Some(key) = cursor.key() {
                        if let Some(key) = key.as_string()
                            && let Some(value) = cursor.value()
                            && let Ok(meta) = serde_wasm_bindgen::from_value::<MetaRecord>(value)
                        {
                            metas.insert(key, meta);
                        }
                        cursor.advance(1).await?;
                    }
                }

                let mut entries = Vec::with_capacity(keys.len());
                for key in keys {
                    let info =
                        entry_info_for_key(&store, &meta_store, &key, metas.remove(&key)).await?;
                    entries.push((key, info));
                }

                Ok(ScanPage {
                    entries,
                    next_start_after,
                    skip_prefix: scan_skip_prefix,
                    done,
                })
            }
        })
        .await
    }

    /// Copies the record at `from` to `to` in one transaction, returning the
    /// new entry's metadata.
    pub(crate) async fn copy(
        &self,
        from: &str,
        to: &str,
        if_not_exists: bool,
        if_match: Option<&str>,
    ) -> opendal::Result<Metadata> {
        if from == to {
            return Err(opendal::Error::new(
                ErrorKind::IsSameFile,
                "copy from and to paths are the same",
            )
            .with_context("from", from)
            .with_context("to", to));
        }

        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        let from = from.to_string();
        let to = to.to_string();
        let if_match = if_match.map(str::to_owned);
        self.run_transaction(true, move || {
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let from = from.clone();
            let to = to.clone();
            let if_match = if_match.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let from_key = JsString::from(from.as_str());

                let Some(value) = store.get(&from_key).await? else {
                    return Err(opendal::Error::new(
                        ErrorKind::NotFound,
                        "indexeddb doesn't have this path",
                    )
                    .with_context("from", &from)
                    .into());
                };
                let to_key = JsString::from(to.as_str());
                let target_exists = store.contains(&to_key).await?;
                if if_not_exists && target_exists {
                    return Err(opendal::Error::new(
                        ErrorKind::ConditionNotMatch,
                        "path already exists",
                    )
                    .with_context("to", &to)
                    .into());
                }

                if let Some(want) = if_match.as_deref() {
                    let target_etag = if target_exists {
                        match read_meta(&meta_store, &to_key).await? {
                            Some(meta) => match meta.e {
                                Some(etag) => Some(etag),
                                None => store.get(&to_key).await?.as_ref().and_then(value_etag),
                            },
                            None => store.get(&to_key).await?.as_ref().and_then(value_etag),
                        }
                    } else {
                        None
                    };
                    let matched =
                        want == "*" && target_exists || target_etag.as_deref() == Some(want);
                    if !matched {
                        return Err(opendal::Error::new(
                            ErrorKind::ConditionNotMatch,
                            "the destination entry's etag does not match if_match",
                        )
                        .with_context("to", &to)
                        .into());
                    }
                }

                let source_meta = read_meta(&meta_store, &from_key).await?;
                let size = source_size(&source_meta, &value, from.as_str())?;
                // The source ETag follows the bytes: a copy keeps the same
                // content, so it keeps the same ETag (S3 single-part copy
                // semantics). A legacy record with no sidecar meta derives
                // its ETag from the value here.
                let source_etag = source_meta
                    .as_ref()
                    .and_then(|m| m.e.clone())
                    .or_else(|| value_etag(&value));
                set_record_key(&value, &to)?;
                store.put(&value).await?;

                let modified = js_sys::Date::now();
                // Clone the source etag before it is consumed by the builder
                // chain below; we need a copy to seed the sidecar content
                // metadata for legacy records.
                let source_etag_clone = source_etag.clone();
                let mut meta = EntryMeta::from(source_meta)
                    .with_size(Some(size as u64))
                    .with_last_modified_ms(Some(modified))
                    .with_etag(source_etag);
                // Ensure the sidecar content metadata carries the same etag,
                // matching the rename path so legacy records (no source meta)
                // also get a stored etag and future stats don't need to
                // re-read the value to derive it.
                if let Some(content) = meta.content.as_mut() {
                    content.etag = source_etag_clone;
                } else if let Some(etag) = source_etag_clone {
                    meta.content = Some(MetaContent {
                        etag: Some(etag),
                        ..Default::default()
                    });
                }
                put_meta(&meta_store, &to, size, Some(modified), meta.content.clone()).await?;

                Ok(entry_metadata(&to, &EntryInfo { meta }))
            }
        })
        .await
    }

    /// Moves the record at `from` to `to` in one transaction, preserving the
    /// entry's last modified time.
    pub(crate) async fn rename(&self, from: &str, to: &str) -> opendal::Result<()> {
        if from == to {
            return Err(opendal::Error::new(
                ErrorKind::IsSameFile,
                "rename from and to paths are the same",
            )
            .with_context("from", from)
            .with_context("to", to));
        }

        let object_store_name = self.object_store_name.clone();
        let meta_store_name = self.meta_store_name.clone();
        let from = from.to_string();
        let to = to.to_string();
        self.run_transaction(true, move || {
            let object_store_name = object_store_name.clone();
            let meta_store_name = meta_store_name.clone();
            let from = from.clone();
            let to = to.clone();
            move |txn| async move {
                let store = txn.object_store(object_store_name.as_str())?;
                let meta_store = txn.object_store(meta_store_name.as_str())?;
                let from_key = JsString::from(from.as_str());

                let Some(value) = store.get(&from_key).await? else {
                    return Err(opendal::Error::new(
                        ErrorKind::NotFound,
                        "indexeddb doesn't have this path",
                    )
                    .with_context("from", &from)
                    .into());
                };

                let source_meta = read_meta(&meta_store, &from_key).await?;
                let size = source_size(&source_meta, &value, from.as_str())?;
                let modified_ms = source_meta.as_ref().and_then(|meta| meta.m);
                // Preserve the ETag across a move. A legacy record with no
                // sidecar meta keeps the bytes, so derive its ETag here too.
                let etag = source_meta
                    .as_ref()
                    .and_then(|m| m.e.clone())
                    .or_else(|| value_etag(&value));
                let mut content_meta = source_meta.as_ref().map(MetaContent::from);
                if let Some(content_meta) = content_meta.as_mut() {
                    content_meta.etag = etag;
                } else if let Some(etag) = etag {
                    content_meta = Some(MetaContent {
                        etag: Some(etag),
                        ..Default::default()
                    });
                }
                set_record_key(&value, &to)?;
                store.put(&value).await?;
                store.delete(&from_key).await?;

                put_meta(&meta_store, &to, size, modified_ms, content_meta).await?;
                meta_store.delete(&from_key).await?;

                Ok(())
            }
        })
        .await
    }
}
