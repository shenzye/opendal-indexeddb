# opendal-indexeddb

An unofficial OpenDAL IndexedDB service library.

This crate is production-ready for wasm browser IndexedDB use cases covered by
the test suite. It is validated by 115+ browser-backed behavior tests in Chrome
and CI also runs Firefox, with known operational limits documented below. See
the changelog for the OpenDAL-version tracking policy.

## Target support

The IndexedDB backend implementation is supported only on wasm targets
(`target_family = "wasm"`).

## Quick start

```toml
[dependencies]
opendal = { version = "0.57", default-features = false }
opendal-indexeddb = "0.57"
```

```rust,ignore
// wasm32 only
use opendal::Operator;
use opendal_indexeddb::IndexeddbBuilder;

let op = Operator::new(
    IndexeddbBuilder::default()
        .db_name("my-app")
        .object_store_name("files"),
)?
.finish();

op.write("hello.txt", "hello world").await?;
```

The service can be composed with standard OpenDAL layers such as retrying,
logging, tracing, and metrics layers.

## Multi-tab behavior

Schema upgrades are coordinated inside the current JavaScript realm with an
upgrade lock. Connections are cached for 500 ms and upgrade races retry up to 8
times, which lets other operators in the same page observe newly created object
stores. Idle cached connections are actively closed after the cache TTL, so an
idle page does not keep blocking another tab's required version upgrade.

Other tabs, workers, active operators, or third-party code can still hold older
IndexedDB connections. If such a connection blocks a required version upgrade
and does not close in response to `versionchange`, this service returns an
`Unexpected` error after 10 seconds instead of leaving the operation pending
forever.

## Known limitations

- Objects are stored and transferred as whole IndexedDB records. `read` and
  `write` load the full object into memory, and range reads first load the
  complete record before slicing it because IndexedDB does not support partial
  value reads. This service is not suitable for very large objects.
- `append` is implemented as read-full-object, concatenate, then write-full-
  object. Growing an object from 0 to N bytes by repeated appends has O(N^2)
  total work; see the `indexeddb/append/4KiB/0_to_1MiB` benchmark below.
- Metadata sizes are stored in IndexedDB as `f64`, so sizes above 2^53 bytes are
  not represented exactly. This is far beyond typical browser storage quotas,
  but it is a theoretical format limit.
- Version upgrades are coordinated inside the current JavaScript realm. Idle
  cached connections are closed after the 500 ms cache TTL. Across tabs or with
  active operators and third-party IndexedDB connections, upgrades still depend
  on old connections closing; a blocking connection causes an `Unexpected`
  error after 10 seconds. See "Multi-tab behavior".
- Browser storage quotas are browser-, profile-, storage-mode-, and
  device-dependent. The upstream `indexed-db` 0.4.2 crate does not expose a
  `QuotaExceededError` variant; source inspection shows write request failures
  with that DOMException name fall through its unknown-DOMException path, which
  panics with a message starting with `Unexpected error:` and including
  `QuotaExceededError` before this service can convert it to `opendal::Error`.
  Other IndexedDB infrastructure errors that do reach this service are reported
  as `ErrorKind::Unexpected`.

## Capabilities

This service can be used to:

| Capability | Status | Notes |
| --- | --- | --- |
| `create_dir` | [x] | Creates directory marker entries. |
| `stat` | [x] | Returns content length and last modified time when available. |
| `read` | [x] | Reads whole objects and byte ranges. |
| `write` | [x] | Writes whole objects, including empty objects. |
| `delete` | [x] | Deletes a single object or directory marker, or recursively deletes a directory tree. |
| `list` | [x] | Lists entries under a path. |
| `copy` | [x] | Copies object data and metadata. |
| `rename` | [x] | Moves object data and metadata. |

Enabled extra capabilities:

| Capability | Notes |
| --- | --- |
| `write_can_empty` | Empty objects can be written. |
| `write_can_append` | Append writes create missing objects or append to existing object content. |
| `write_with_if_not_exists` | Fails when the destination already exists. |
| `write_with_content_type` | Stores and returns Content-Type metadata. |
| `write_with_user_metadata` | Stores and returns user metadata. |
| `copy_with_if_not_exists` | Fails when the destination already exists. |
| `copy_with_if_match` | Fails when the existing destination ETag does not match. |
| `list_with_recursive` | Recursive listing is supported. |
| `delete_with_recursive` | Recursive directory deletion is supported. |
| `delete_max_size` | Batch deletion groups up to 100 delete requests per IndexedDB transaction. |
| `list_with_limit` | Limits the number of entries returned by a list. |
| `list_with_start_after` | Resumes listing after a given path. |
| `stat_with_if_match` | Fails when the entry's ETag does not match. |
| `stat_with_if_none_match` | Fails when the entry's ETag matches. |
| `stat_with_if_modified_since` | Fails when the entry has not changed since the given time. |
| `stat_with_if_unmodified_since` | Fails when the entry has changed since the given time. |
| `read_with_if_match` | Fails when the entry's ETag does not match. |
| `read_with_if_none_match` | Fails when the entry's ETag matches. |
| `read_with_if_modified_since` | Fails when the entry has not changed since the given time. |
| `read_with_if_unmodified_since` | Fails when the entry has changed since the given time. |
| `write_with_if_match` | Writes only when the existing ETag matches. |
| `write_with_if_none_match` | Writes only when the existing ETag does not match. |

Unsupported capabilities:

| Category | Capabilities | Notes |
| --- | --- | --- |
| target | Non-wasm backend implementation, `Configurator` integration | On non-wasm targets, this crate can still be used to handle `IndexeddbConfig` values, such as serializing, deserializing, or passing configuration data around. |
| `presign` | `presign_read`, `presign_stat`, `presign_write`, `presign_delete` | IndexedDB has no remote request to sign. |
| write metadata | `write_with_cache_control`, `write_with_content_disposition`, `write_with_content_encoding` | IndexedDB has no HTTP response header behavior, so these response-oriented metadata fields are not stored or returned. |
| response header override | `stat_with_override_cache_control`, `stat_with_override_content_disposition`, `stat_with_override_content_type`, `read_with_override_cache_control`, `read_with_override_content_disposition`, `read_with_override_content_type` | These options are only meaningful for signed remote requests that can override HTTP response headers. IndexedDB has no presigned HTTP response to override. |
| write size limit | `write_total_max_size` | IndexedDB quota is browser-, profile-, storage-mode-, and device-dependent, so this backend cannot advertise a stable maximum write size. |
| remote multipart or segmented operations | `write_can_multi`, `copy_can_multi`, `write_multi_max_size`, `write_multi_min_size`, `copy_multi_max_size`, `copy_multi_min_size` | This backend stores whole objects in IndexedDB and does not support remote multipart upload or server-side segmented copy. |
| versioning | `stat_with_version`, `read_with_version`, `delete_with_version`, `list_with_versions`, `list_with_deleted` | This backend keeps one current record per path and does not keep object versions or delete markers. |
| `shared` | `shared` | This backend does not provide shared access URLs, tokens, or related shared-access semantics. |

## Testing

Test command (wasm-bindgen-cli 0.2.121):
```
wasm-pack test --headless --firefox
wasm-pack test --headless --chrome
```

Performance benchmarks are opt-in so regular tests stay focused and fast:
```
WASM_BINDGEN_TEST_TIMEOUT=120 wasm-pack test --release --headless --firefox . --features perf-tests -- --bench
WASM_BINDGEN_TEST_TIMEOUT=120 wasm-pack test --release --headless --chrome . --features perf-tests -- --bench
```

The quota exhaustion probe is ignored by default because it intentionally fills
browser storage and can leave large test databases behind. Run it only with a
throwaway, storage-limited browser profile:
```
WASM_BINDGEN_TEST_TIMEOUT=600 wasm-pack test --headless --chrome . -- --include-ignored manual_quota_exhaustion_probe
```

The benchmark suite runs in a browser-backed wasm environment and currently
covers steady-state 4 KiB and 1 MiB reads/writes, stat, flat list, recursive
list, 100-entry batch deletion, a 4 KiB range read from a 1 MiB object, and 4 KiB
append growth from 0 to 1 MiB. The
first database creation/upgrade is performed during setup and is not part of
the measured steady-state read/write/list/stat operations. Delete benchmarks
prepare their input objects outside the timed section, so the reported time
covers only `delete_iter`.

## Performance results

Measured on 2026-07-05 with `wasm-pack 0.15.0`,
`wasm-bindgen 0.2.121`, and `rustc 1.96.1`. Benchmarks ran with Criterion's
current suite settings: release build, sample size 15, 200 ms warm-up, and
1.5 s measurement time. Times below are lower / point / upper estimates; lower
is better. The repository's `webdriver.json` raises WebDriver script/page-load
timeouts for longer browser-backed benchmark runs.

| Benchmark | Scope | Chromium 150.0.7871.46 | Firefox 152.0.1 |
| --- | --- | --- | --- |
| `indexeddb/write/4KiB` | Write one 4 KiB object | 262.99 us / 267.78 us / 273.72 us | 188.74 us / 227.28 us / 279.44 us |
| `indexeddb/read/4KiB` | Read one 4 KiB object | 140.94 us / 144.78 us / 148.41 us | 137.67 us / 140.41 us / 144.08 us |
| `indexeddb/write/1MiB` | Write one 1 MiB object | 4.1240 ms / 4.2224 ms / 4.3370 ms | 8.6961 ms / 8.9052 ms / 9.1555 ms |
| `indexeddb/read/1MiB` | Read one 1 MiB object | 1.2979 ms / 1.3403 ms / 1.4139 ms | 3.8152 ms / 3.9181 ms / 4.0754 ms |
| `indexeddb/read/1MiB-range-4KiB` | Read one 4 KiB range from a 1 MiB object | 1.2535 ms / 1.2795 ms / 1.3052 ms | 3.9047 ms / 3.9847 ms / 4.0847 ms |
| `indexeddb/append/4KiB/0_to_1MiB` | Append 256 4 KiB chunks from 0 to 1 MiB | 757.03 ms, 1.32 MiB/s, 2.957 ms avg append | 1.2178 s, 0.82 MiB/s, 4.757 ms avg append |
| `indexeddb/stat/existing` | Stat one existing object | 120.42 us / 130.43 us / 150.06 us | 111.55 us / 112.80 us / 114.29 us |
| `indexeddb/list/flat-512` | List 512 flat entries | 22.752 ms / 24.287 ms / 26.099 ms | 7.0945 ms / 8.7641 ms / 11.372 ms |
| `indexeddb/list/recursive-512` | Recursively list 512 nested entries | 23.023 ms / 27.389 ms / 34.916 ms | 6.8489 ms / 8.1353 ms / 10.302 ms |
| `indexeddb/delete_iter/100` | Delete 100 pre-created objects | 18.230 ms / 20.724 ms / 23.545 ms | 8.8370 ms / 17.718 ms / 34.708 ms |
