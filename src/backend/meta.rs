use js_sys::JsString;
use js_sys::wasm_bindgen::{JsCast, JsValue};
use opendal::raw::{OpRead, OpWrite, Timestamp};
use opendal::{EntryMode, ErrorKind, Metadata};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub(super) struct KeyAndValue {
    pub(super) k: String,
    #[serde(with = "serde_bytes")]
    pub(super) v: Vec<u8>,
}

/// Sidecar metadata kept in an internal meta store so that stat and
/// list never need to load file contents.
///
/// Numbers are stored as f64 rather than u64 because serde_wasm_bindgen maps
/// u64 to BigInt, which cannot be used as an IndexedDB key and is awkward for
/// JS consumers; f64 is exact up to 2^53.
#[derive(Serialize, Deserialize)]
pub(super) struct MetaRecord {
    pub(super) k: String,
    /// Content length in bytes.
    pub(super) l: f64,
    /// Last modified time in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) m: Option<f64>,
    /// ETag, including the surrounding quotes (per OpenDAL's ETag convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) e: Option<String>,
    /// Content-Type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ct: Option<String>,
    /// User metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) um: Option<HashMap<String, String>>,
}

impl MetaRecord {
    pub(super) fn new(k: String, l: f64, m: Option<f64>, content: Option<MetaContent>) -> Self {
        let content = content.unwrap_or_default();
        Self {
            k,
            l,
            m,
            e: content.etag,
            ct: content.content_type,
            um: content.user_metadata,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct MetaContent {
    pub(super) etag: Option<String>,
    pub(super) content_type: Option<String>,
    pub(super) user_metadata: Option<HashMap<String, String>>,
}

impl From<&MetaRecord> for MetaContent {
    fn from(value: &MetaRecord) -> Self {
        Self {
            etag: value.e.clone(),
            content_type: value.ct.clone(),
            user_metadata: value.um.clone(),
        }
    }
}

impl MetaContent {
    pub(super) fn merge(&mut self, value: MetaContent) {
        if value.content_type.is_some() {
            self.content_type = value.content_type;
        }
        if value.user_metadata.is_some() {
            self.user_metadata = value.user_metadata;
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct EntryMeta {
    pub(super) size: Option<u64>,
    pub(super) last_modified_ms: Option<f64>,
    pub(super) etag: Option<String>,
    pub(super) content: Option<MetaContent>,
}

impl EntryMeta {
    pub(super) fn from_size(size: Option<u64>) -> Self {
        Self {
            size,
            ..Default::default()
        }
    }

    pub(super) fn with_size(mut self, size: Option<u64>) -> Self {
        self.size = size;
        self
    }

    pub(super) fn with_last_modified_ms(mut self, last_modified_ms: Option<f64>) -> Self {
        self.last_modified_ms = last_modified_ms;
        self
    }

    pub(super) fn with_etag(mut self, etag: Option<String>) -> Self {
        self.etag = etag;
        self
    }
}

impl From<MetaRecord> for EntryMeta {
    fn from(value: MetaRecord) -> Self {
        let content = MetaContent::from(&value);
        Self {
            size: Some(value.l as u64),
            last_modified_ms: value.m,
            etag: value.e,
            content: Some(content),
        }
    }
}

impl From<Option<MetaRecord>> for EntryMeta {
    fn from(value: Option<MetaRecord>) -> Self {
        match value {
            Some(value) => value.into(),
            None => Self::default(),
        }
    }
}

pub(super) fn entry_meta_with_etag_fallback(meta: MetaRecord, etag: Option<String>) -> EntryMeta {
    let mut entry = EntryMeta::from(meta);
    if entry.etag.is_none() {
        entry.etag = etag.clone();
        if let Some(content) = entry.content.as_mut() {
            content.etag = etag;
        }
    }
    entry
}

pub(super) struct EntryInfo {
    pub(super) meta: EntryMeta,
}

pub(super) struct ScanPageRequest<'a> {
    pub(super) prefix: &'a str,
    pub(super) start_after: Option<&'a str>,
    pub(super) skip_prefix: Option<&'a str>,
    pub(super) limit: usize,
}

pub(super) struct ScanPage {
    pub(super) entries: Vec<(String, EntryInfo)>,
    pub(super) next_start_after: Option<String>,
    pub(super) skip_prefix: Option<String>,
    pub(super) done: bool,
}

/// Rewrites a data record's `k` field so it can be stored under a new key
/// (the stores use `k` as their key path).
pub(super) fn set_record_key(
    record: &JsValue,
    key: &str,
) -> Result<(), indexed_db::Error<opendal::Error>> {
    js_sys::Reflect::set(record, &JsValue::from_str("k"), &JsValue::from_str(key)).map_err(
        |_| {
            opendal::Error::new(
                ErrorKind::Unexpected,
                "failed to rewrite indexeddb record key",
            )
        },
    )?;
    Ok(())
}

pub(super) async fn read_meta(
    meta_store: &indexed_db::ObjectStore<opendal::Error>,
    key: &JsString,
) -> Result<Option<MetaRecord>, indexed_db::Error<opendal::Error>> {
    Ok(meta_store
        .get(key)
        .await?
        .and_then(|value| serde_wasm_bindgen::from_value::<MetaRecord>(value).ok()))
}

pub(super) async fn entry_info_for_key(
    store: &indexed_db::ObjectStore<opendal::Error>,
    meta_store: &indexed_db::ObjectStore<opendal::Error>,
    key: &str,
    prefetched_meta: Option<MetaRecord>,
) -> Result<EntryInfo, indexed_db::Error<opendal::Error>> {
    let key_string = JsString::from(key);
    if let Some(meta) = match prefetched_meta {
        Some(meta) => Some(meta),
        None => read_meta(meta_store, &key_string).await?,
    } {
        let etag = if meta.e.is_none() {
            let value = store.get(&key_string).await?;
            value.as_ref().and_then(value_etag)
        } else {
            None
        };
        return Ok(EntryInfo {
            meta: entry_meta_with_etag_fallback(meta, etag),
        });
    }

    if key.ends_with('/') {
        return Ok(EntryInfo {
            meta: EntryMeta::default(),
        });
    }

    match store.get(&key_string).await? {
        Some(value) => Ok(EntryInfo {
            meta: EntryMeta::from_size(value_byte_length(&value)).with_etag(value_etag(&value)),
        }),
        None => Ok(EntryInfo {
            meta: EntryMeta::default(),
        }),
    }
}

pub(super) async fn put_meta(
    meta_store: &indexed_db::ObjectStore<opendal::Error>,
    key: &str,
    length: f64,
    modified_ms: Option<f64>,
    content: Option<MetaContent>,
) -> Result<(), indexed_db::Error<opendal::Error>> {
    let meta = serde_wasm_bindgen::to_value(&MetaRecord::new(
        key.to_string(),
        length,
        modified_ms,
        content,
    ))
    .map_err(|err| opendal::Error::new(ErrorKind::Unexpected, err.to_string()))?;
    meta_store.put(&meta).await?;
    Ok(())
}

fn record_value_as_uint8_array(
    record: &JsValue,
    path: &str,
) -> Result<js_sys::Uint8Array, indexed_db::Error<opendal::Error>> {
    let v = js_sys::Reflect::get(record, &JsValue::from_str("v")).map_err(|_| {
        opendal::Error::new(
            ErrorKind::Unexpected,
            "failed to read indexeddb record value",
        )
        .with_context("path", path.to_string())
    })?;
    let arr: js_sys::Uint8Array = v.dyn_into().map_err(|_| {
        opendal::Error::new(
            ErrorKind::Unexpected,
            "indexeddb record value is not a byte array",
        )
        .with_context("path", path.to_string())
    })?;
    Ok(arr)
}

pub(super) fn record_bytes(
    record: &JsValue,
    path: &str,
) -> Result<Vec<u8>, indexed_db::Error<opendal::Error>> {
    Ok(record_value_as_uint8_array(record, path)?.to_vec())
}

fn clamp_read_range(range: ReadRange, total_size: u64) -> std::ops::Range<usize> {
    let start = range.offset.min(total_size) as usize;
    let end = match range.size {
        Some(size) => range.offset.saturating_add(size).min(total_size),
        None => total_size,
    } as usize;
    start..end
}

pub(super) fn record_bytes_range(
    record: &JsValue,
    path: &str,
    range: ReadRange,
    total_size: u64,
) -> Result<Vec<u8>, indexed_db::Error<opendal::Error>> {
    let arr = record_value_as_uint8_array(record, path)?;
    let range = clamp_read_range(range, total_size);
    Ok(arr.subarray(range.start as u32, range.end as u32).to_vec())
}

/// Content length of a source record: validate the value is bytes first, then
/// use sidecar meta if present or the measured length for legacy records.
pub(super) fn source_size(
    meta: &Option<MetaRecord>,
    value: &JsValue,
    path: &str,
) -> Result<f64, indexed_db::Error<opendal::Error>> {
    let measured = u64::from(record_value_as_uint8_array(value, path)?.byte_length()) as f64;
    Ok(match meta {
        Some(meta) => meta.l,
        None => measured,
    })
}

/// Reads the byte length of a data record's `v` field without deserializing
/// the payload. Used for records written before the meta store existed.
pub(super) fn value_byte_length(record: &JsValue) -> Option<u64> {
    let v = js_sys::Reflect::get(record, &JsValue::from_str("v")).ok()?;
    let arr: js_sys::Uint8Array = v.dyn_into().ok()?;
    Some(u64::from(arr.byte_length()))
}

/// Computes a quoted ETag string for `bytes` (BLAKE3, hex). The quotes
/// are part of the value, matching how OpenDAL stores/returns ETags, so a
/// caller doing `op.write_with(...).if_match(meta.etag())` round-trips exactly.
pub(super) fn etag_from_bytes(bytes: &[u8]) -> String {
    format!("\"{}\"", blake3::hash(bytes).to_hex())
}

/// Computes an ETag from a data record's `v` field without deserializing it.
/// Used for records written before the meta store existed, so they too carry an
/// ETag for conditional operations.
pub(super) fn value_etag(record: &JsValue) -> Option<String> {
    let v = js_sys::Reflect::get(record, &JsValue::from_str("v")).ok()?;
    let arr: js_sys::Uint8Array = v.dyn_into().ok()?;
    // Copy into a Vec so we can hash it; values are typically small and this
    // path only runs for legacy records that have no sidecar meta.
    let bytes = arr.to_vec();
    Some(etag_from_bytes(&bytes))
}

pub(super) fn entry_metadata(path: &str, info: &EntryInfo) -> Metadata {
    let mode = EntryMode::from_path(path);
    let mut meta = Metadata::new(mode);
    if mode.is_file()
        && let Some(size) = info.meta.size
    {
        meta.set_content_length(size);
    }
    if let Some(ts) = last_modified(info) {
        meta.set_last_modified(ts);
    }
    if let Some(etag) = &info.meta.etag {
        meta.set_etag(etag);
    }
    if let Some(content) = &info.meta.content {
        apply_content_metadata(&mut meta, content);
    }
    meta
}

/// The entry's ETag, including its surrounding quotes, when known.
///
/// Records written before the meta store existed are assigned an ETag lazily
/// from their stored value (see `value_etag`), so legacy entries still take part
/// in conditional operations.
fn etag(info: &EntryInfo) -> Option<&str> {
    info.meta.etag.as_deref()
}

/// Evaluates `if_match` / `if_none_match` against an entry's ETag.
///
/// - `if_match` is satisfied when the entry's ETag equals it, or when it is
///   `*` and the entry exists. Anything else is a mismatch.
/// - `if_none_match` is satisfied when the entry's ETag differs from it, or
///   when it is `*` and the entry does *not* exist — but this helper is only
///   called for entries that do exist, so `*` always mismatches here.
///
/// Mismatches are reported as [`ErrorKind::ConditionNotMatch`], per the OpenDAL
/// option docs.
pub(super) fn check_etag(
    info: &EntryInfo,
    if_match: Option<&str>,
    if_none_match: Option<&str>,
) -> opendal::Result<()> {
    if let Some(want) = if_match {
        let matched = want == "*" || etag(info) == Some(want);
        if !matched {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "the entry's etag does not match if_match",
            ));
        }
    }
    if let Some(want) = if_none_match {
        let matched = want == "*" || etag(info) == Some(want);
        if matched {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "the entry's etag matches if_none_match",
            ));
        }
    }
    Ok(())
}

/// The entry's last modified time, when the sidecar meta carries one.
///
/// Records written before the meta store existed have no `m`, so this returns
/// `None` — the condition checks below treat an unknown mtime as "we cannot
/// prove it was modified", matching the OpenDAL option docs.
fn last_modified(info: &EntryInfo) -> Option<Timestamp> {
    info.meta
        .last_modified_ms
        .and_then(|ms| Timestamp::from_millisecond(ms as i64).ok())
}

/// Evaluates `if_modified_since` / `if_unmodified_since` against an entry's
/// last modified time.
///
/// - `if_modified_since` returns the entry only when it changed after the given
///   time; anything at or before it (or with no known mtime) is "not modified".
/// - `if_unmodified_since` returns the entry only when it did not change after
///   the given time; a later mtime means it was modified.
///
/// Both report a mismatch as [`ErrorKind::ConditionNotMatch`], per the OpenDAL
/// option docs.
pub(super) fn check_modified(
    info: &EntryInfo,
    if_modified_since: Option<Timestamp>,
    if_unmodified_since: Option<Timestamp>,
) -> opendal::Result<()> {
    // When the mtime is unknown (e.g. legacy records written before the
    // sidecar meta store existed), we cannot disprove modification, so both
    // conditions are satisfied and the operation proceeds. The root stat path
    // is handled separately and never reaches here.
    if let Some(last) = last_modified(info) {
        if let Some(since) = if_modified_since
            && last <= since
        {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "the entry has not been modified since the given time",
            ));
        }
        if let Some(until) = if_unmodified_since
            && last > until
        {
            return Err(opendal::Error::new(
                ErrorKind::ConditionNotMatch,
                "the entry has been modified since the given time",
            ));
        }
    }
    Ok(())
}

fn apply_content_metadata(meta: &mut Metadata, content: &MetaContent) {
    if let Some(value) = &content.content_type {
        meta.set_content_type(value);
    }
    if let Some(value) = &content.user_metadata {
        *meta = meta.clone().with_user_metadata(value.clone());
    }
}

#[derive(Clone)]
pub(super) struct WriteArgs {
    pub(super) append: bool,
    pub(super) if_not_exists: bool,
    pub(super) if_match: Option<String>,
    pub(super) if_none_match: Option<String>,
    pub(super) meta: Option<MetaContent>,
}

impl From<&OpWrite> for WriteArgs {
    fn from(value: &OpWrite) -> Self {
        Self {
            append: value.append(),
            if_not_exists: value.if_not_exists(),
            if_match: value.if_match().map(ToString::to_string),
            if_none_match: value.if_none_match().map(ToString::to_string),
            meta: Some(MetaContent {
                content_type: value.content_type().map(ToString::to_string),
                user_metadata: value.user_metadata().cloned(),
                ..Default::default()
            }),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ReadRange {
    offset: u64,
    size: Option<u64>,
}

pub(super) struct ReadArgs {
    pub(super) if_match: Option<String>,
    pub(super) if_none_match: Option<String>,
    pub(super) if_modified_since: Option<Timestamp>,
    pub(super) if_unmodified_since: Option<Timestamp>,
    pub(super) range: ReadRange,
}

impl From<&OpRead> for ReadArgs {
    fn from(value: &OpRead) -> Self {
        let range = value.range();
        Self {
            if_match: value.if_match().map(ToString::to_string),
            if_none_match: value.if_none_match().map(ToString::to_string),
            if_modified_since: value.if_modified_since(),
            if_unmodified_since: value.if_unmodified_since(),
            range: ReadRange {
                offset: range.offset(),
                size: range.size(),
            },
        }
    }
}
