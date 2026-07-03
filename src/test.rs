use crate::IndexeddbBuilder;
use crate::backend::IndexeddbCore;
use crate::config::IndexeddbConfig;
use futures_util::{
    future::{Either, select},
    pin_mut,
};
use gloo_timers::future::{TimeoutFuture, sleep};
use opendal::raw::OpDelete;
use opendal::{Configurator, ErrorKind, Metadata, Operator};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use wasm_bindgen_test::{console_log, wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

static TEST_DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

async fn fresh_test_db_name(label: &str) -> String {
    let id = TEST_DB_COUNTER.fetch_add(1, Ordering::SeqCst);
    let name = format!(
        "opendal_indexeddb_test_{label}_{}_{id}",
        js_sys::Date::now() as u64
    );
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();
    factory.delete_database(name.as_str()).await.unwrap();
    name
}

#[wasm_bindgen_test]
async fn capabilities_match_readme() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("capability_readme_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let cap = op.info().native_capability();
    assert!(cap.create_dir);
    assert!(cap.stat);
    assert!(cap.read);
    assert!(cap.write);
    assert!(cap.delete);
    assert!(cap.list);
    assert!(cap.copy);
    assert!(cap.rename);

    assert!(cap.write_can_empty);
    assert!(cap.write_can_append);
    assert!(cap.write_with_if_not_exists);
    assert!(cap.write_with_content_type);
    assert!(!cap.write_with_content_disposition);
    assert!(!cap.write_with_content_encoding);
    assert!(!cap.write_with_cache_control);
    assert!(cap.write_with_user_metadata);
    assert!(cap.copy_with_if_not_exists);
    assert!(cap.copy_with_if_match);
    assert!(cap.list_with_recursive);
    assert!(cap.delete_with_recursive);
    assert_eq!(cap.delete_max_size, Some(100));
    assert!(cap.list_with_limit);
    assert!(cap.list_with_start_after);
    assert!(cap.stat_with_if_match);
    assert!(cap.stat_with_if_none_match);
    assert!(cap.stat_with_if_modified_since);
    assert!(cap.stat_with_if_unmodified_since);
    assert!(cap.read_with_if_match);
    assert!(cap.read_with_if_none_match);
    assert!(cap.read_with_if_modified_since);
    assert!(cap.read_with_if_unmodified_since);
    assert!(cap.write_with_if_match);
    assert!(cap.write_with_if_none_match);

    assert!(!cap.presign);
    assert!(!cap.presign_read);
    assert!(!cap.presign_stat);
    assert!(!cap.presign_write);
    assert!(!cap.presign_delete);
    assert!(!cap.stat_with_override_cache_control);
    assert!(!cap.stat_with_override_content_disposition);
    assert!(!cap.stat_with_override_content_type);
    assert!(!cap.read_with_override_cache_control);
    assert!(!cap.read_with_override_content_disposition);
    assert!(!cap.read_with_override_content_type);
    assert!(!cap.write_can_multi);
    assert_eq!(cap.write_total_max_size, None);
    assert_eq!(cap.write_multi_max_size, None);
    assert_eq!(cap.write_multi_min_size, None);
    assert!(!cap.copy_can_multi);
    assert_eq!(cap.copy_multi_max_size, None);
    assert_eq!(cap.copy_multi_min_size, None);
    assert!(!cap.stat_with_version);
    assert!(!cap.read_with_version);
    assert!(!cap.delete_with_version);
    assert!(!cap.list_with_versions);
    assert!(!cap.list_with_deleted);
    assert!(!cap.shared);
}

#[wasm_bindgen_test]
async fn concurrent_rw_test() {
    let session_store_config = IndexeddbConfig {
        db_name: Some(fresh_test_db_name("concurrent_rw_test").await),
        object_store_name: None,
        root: None,
    };
    let builder = session_store_config.into_builder();

    let op = Operator::new(builder).unwrap().finish();
    op.write("hello", "world").await.unwrap();

    let done = Arc::new(AtomicUsize::new(0));
    for _ in 0..1000 {
        spawn_local({
            let op = op.clone();
            let done = done.clone();
            async move {
                op.write("hello", "world").await.unwrap();
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
        spawn_local({
            let op = op.clone();
            let done = done.clone();
            async move {
                assert_eq!(op.read("hello").await.unwrap().to_vec(), b"world");
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    while done.load(Ordering::SeqCst) != 2000 {
        sleep(Duration::from_secs(1)).await;
    }
}

#[wasm_bindgen_test]
async fn read_range_is_clamped_to_content_length() {
    let session_store_config = IndexeddbConfig {
        db_name: Some(fresh_test_db_name("read_range_clamped_test").await),
        object_store_name: None,
        root: None,
    };
    let builder = session_store_config.into_builder();

    let op = Operator::new(builder).unwrap().finish();
    op.write("short", "hello").await.unwrap();

    assert_eq!(
        op.read_with("short").range(1..4).await.unwrap().to_vec(),
        b"ell"
    );
    assert_eq!(
        op.read_with("short").range(0..5).await.unwrap().to_vec(),
        b"hello"
    );
    assert_eq!(
        op.read_with("short").range(10..).await.unwrap().to_vec(),
        b""
    );

    let err = op.read_with("short").range(0..10).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
}

#[wasm_bindgen_test]
async fn write_empty_object_and_delete_directory_marker() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("empty_and_dir_marker_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("empty", Vec::<u8>::new()).await.unwrap();
    let meta = op.stat("empty").await.unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.content_length(), 0);
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");

    op.create_dir("dir/").await.unwrap();
    op.write("dir/file", "kept").await.unwrap();
    assert!(op.stat("dir/").await.unwrap().is_dir());

    op.delete("dir/").await.unwrap();
    let err = op.stat("dir/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op.read("dir/file").await.unwrap().to_vec(), b"kept");
}

#[wasm_bindgen_test]
async fn second_store_on_same_db_can_upgrade() {
    let db_name = fresh_test_db_name("upgrade_test").await;
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("store_a".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    op_a.write("a", "1").await.unwrap();

    // Adding store_b requires a version upgrade of the same database. Before
    // connections were closed after each operation, the connection leaked by
    // the write above blocked this upgrade forever.
    let op_b = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("store_b".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    op_b.write("b", "2").await.unwrap();

    assert_eq!(op_a.read("a").await.unwrap().to_vec(), b"1");
    assert_eq!(op_b.read("b").await.unwrap().to_vec(), b"2");
}

#[wasm_bindgen_test]
async fn cached_connection_does_not_block_second_store_upgrade() {
    let db_name = fresh_test_db_name("cached_upgrade_test").await;
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("cached_store_a".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    op_a.write("a", "1").await.unwrap();
    assert_eq!(op_a.stat("a").await.unwrap().content_length(), 1);

    let op_b = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("cached_store_b".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    op_b.write("b", "2").await.unwrap();

    assert_eq!(op_a.read("a").await.unwrap().to_vec(), b"1");
    assert_eq!(op_b.read("b").await.unwrap().to_vec(), b"2");
}

#[wasm_bindgen_test]
async fn idle_cached_connection_is_closed_after_ttl() {
    let db_name = fresh_test_db_name("idle_cache_close_test").await;
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("idle_store_a".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    op_a.write("a", "1").await.unwrap();

    TimeoutFuture::new(700).await;

    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();
    let current = factory.open_latest_version(db_name.as_str()).await.unwrap();
    let next_version = current.version() + 1;
    current.close();

    let open = factory.open(db_name.as_str(), next_version, |_evt| async move { Ok(()) });
    let timeout = TimeoutFuture::new(2_000);
    pin_mut!(open);
    pin_mut!(timeout);
    let upgraded = match select(open, timeout).await {
        Either::Left((result, _)) => result.unwrap(),
        Either::Right((_, _)) => panic!("idle cached connection blocked version upgrade"),
    };
    upgraded.close();
}

#[wasm_bindgen_test]
async fn blocked_upgrade_times_out_instead_of_hanging() {
    let db_name = fresh_test_db_name("blocked_upgrade_timeout_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();
    let blocking_db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("blocking")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let err = op.write("file", "value").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    assert!(err.to_string().contains("version upgrade timed out"));

    blocking_db.close();
}

#[wasm_bindgen_test]
async fn concurrent_stores_on_same_db_coordinate_upgrade() {
    let db_name = fresh_test_db_name("upgrade_burst_test").await;
    let total = 24;
    let done = Arc::new(AtomicUsize::new(0));
    let errors = Rc::new(RefCell::new(Vec::new()));

    for idx in 0..total {
        let op = Operator::new(
            IndexeddbConfig {
                db_name: Some(db_name.to_string()),
                object_store_name: Some(format!("burst_store_{idx}")),
                root: None,
            }
            .into_builder(),
        )
        .unwrap()
        .finish();

        spawn_local({
            let done = done.clone();
            let errors = errors.clone();
            async move {
                let path = format!("file_{idx}");
                let value = format!("value_{idx}");
                let result: opendal::Result<()> = async {
                    op.write(path.as_str(), value.clone().into_bytes()).await?;
                    let actual = op.read(path.as_str()).await?;
                    if actual.to_vec() != value.as_bytes() {
                        return Err(opendal::Error::new(
                            ErrorKind::Unexpected,
                            "indexeddb burst store read returned unexpected content",
                        ));
                    }
                    Ok(())
                }
                .await;

                if let Err(err) = result {
                    errors.borrow_mut().push(err.to_string());
                }
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    while done.load(Ordering::SeqCst) != total {
        sleep(Duration::from_millis(10)).await;
    }

    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
}

#[wasm_bindgen_test]
async fn list_and_stat_carry_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("meta_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "hello").await.unwrap();
    op.write("dir/b", "worlds!").await.unwrap();
    op.create_dir("dir/sub/").await.unwrap();

    let meta = op.stat("dir/a").await.unwrap();
    assert!(meta.is_file());
    assert_eq!(meta.content_length(), 5);
    assert!(meta.last_modified().is_some());

    let entries = op.list("dir/").await.unwrap();
    let a = entries.iter().find(|e| e.path() == "dir/a").unwrap();
    assert_eq!(a.metadata().content_length(), 5);
    assert!(a.metadata().last_modified().is_some());
    let b = entries.iter().find(|e| e.path() == "dir/b").unwrap();
    assert_eq!(b.metadata().content_length(), 7);
    let sub = entries.iter().find(|e| e.path() == "dir/sub/").unwrap();
    assert!(sub.metadata().is_dir());
}

#[wasm_bindgen_test]
async fn copy_overwrites_and_carries_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("copy_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("src", "hello").await.unwrap();
    op.copy("src", "dst").await.unwrap();
    assert_eq!(op.read("dst").await.unwrap().to_vec(), b"hello");
    let meta = op.stat("dst").await.unwrap();
    assert_eq!(meta.content_length(), 5);
    assert!(meta.last_modified().is_some());
    // the source is untouched
    assert_eq!(op.read("src").await.unwrap().to_vec(), b"hello");

    // copy overwrites an existing destination
    op.write("existing", "old contents").await.unwrap();
    op.copy("src", "existing").await.unwrap();
    assert_eq!(op.read("existing").await.unwrap().to_vec(), b"hello");
    assert_eq!(op.stat("existing").await.unwrap().content_length(), 5);

    // copying a missing path fails
    let err = op.copy("missing", "dst").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // if_not_exists refuses an existing destination but allows a fresh one
    let err = op
        .copy_with("src", "existing")
        .if_not_exists(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.copy_with("src", "fresh")
        .if_not_exists(true)
        .await
        .unwrap();
    assert_eq!(op.read("fresh").await.unwrap().to_vec(), b"hello");
}

#[wasm_bindgen_test]
async fn rename_moves_data_and_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("rename_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("from", "hello").await.unwrap();
    let src_meta = op.stat("from").await.unwrap();
    op.rename("from", "to").await.unwrap();
    assert_eq!(op.read("to").await.unwrap().to_vec(), b"hello");
    let meta = op.stat("to").await.unwrap();
    assert_eq!(meta.content_length(), 5);
    // rename preserves the last modified time
    assert_eq!(meta.last_modified(), src_meta.last_modified());
    let err = op.stat("from").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // renaming a missing path fails
    let err = op.rename("missing", "to").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // rename overwrites an existing destination, metadata included
    op.write("a", "aaaa").await.unwrap();
    op.write("b", "b").await.unwrap();
    op.rename("a", "b").await.unwrap();
    assert_eq!(op.read("b").await.unwrap().to_vec(), b"aaaa");
    assert_eq!(op.stat("b").await.unwrap().content_length(), 4);
}

#[wasm_bindgen_test]
async fn copy_and_rename_directory_markers_are_rejected_by_operator() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("copy_rename_dir_marker_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    op.write("dir/file", "child").await.unwrap();

    let err = op.copy("dir/", "copy/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);
    let err = op.stat("copy/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    let err = op.rename("dir/", "moved/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);
    let err = op.stat("moved/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    assert!(op.stat("dir/").await.unwrap().is_dir());
    assert_eq!(op.read("dir/file").await.unwrap().to_vec(), b"child");
}

#[wasm_bindgen_test]
async fn write_with_if_not_exists() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("if_not_exists_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("file", "first")
        .if_not_exists(true)
        .await
        .unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"first");

    let err = op
        .write_with("file", "second")
        .if_not_exists(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    // the failed write left the content untouched
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"first");

    // a plain write still overwrites
    op.write("file", "second").await.unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"second");
}

#[wasm_bindgen_test]
async fn write_with_append_appends_and_preserves_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("append_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let written = op
        .write_with("file", "hello")
        .content_type("text/plain")
        .user_metadata([("owner".to_string(), "opendal".to_string())])
        .await
        .unwrap();
    let old_etag = written.etag().unwrap().to_string();

    let appended = op.write_with("file", " world").append(true).await.unwrap();
    assert_eq!(appended.content_length(), 11);
    assert_ne!(appended.etag().unwrap(), old_etag);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"hello world");

    let meta = op.stat("file").await.unwrap();
    assert_eq!(meta.content_length(), 11);
    assert_eq!(meta.content_type(), Some("text/plain"));
    assert_eq!(
        meta.user_metadata()
            .and_then(|metadata| metadata.get("owner").map(String::as_str)),
        Some("opendal")
    );
    assert_eq!(meta.etag(), appended.etag());

    // Appending to a missing path creates it, like a normal write.
    op.write_with("fresh", "new").append(true).await.unwrap();
    assert_eq!(op.read("fresh").await.unwrap().to_vec(), b"new");
}

#[wasm_bindgen_test]
async fn append_with_metadata_overrides_existing_metadata_fields() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("append_metadata_override_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("file", "hello")
        .content_type("text/plain")
        .user_metadata([
            ("owner".to_string(), "opendal".to_string()),
            ("keep".to_string(), "old".to_string()),
        ])
        .await
        .unwrap();

    let appended = op
        .write_with("file", " world")
        .append(true)
        .content_type("application/json")
        .user_metadata([("purpose".to_string(), "append".to_string())])
        .await
        .unwrap();

    assert_eq!(appended.content_length(), 11);
    assert_eq!(appended.content_type(), Some("application/json"));
    assert_eq!(
        appended
            .user_metadata()
            .and_then(|metadata| metadata.get("purpose").map(String::as_str)),
        Some("append")
    );
    assert_eq!(
        appended
            .user_metadata()
            .and_then(|metadata| metadata.get("owner").map(String::as_str)),
        None
    );

    assert_eq!(op.read("file").await.unwrap().to_vec(), b"hello world");
    let stat = op.stat("file").await.unwrap();
    assert_eq!(stat.content_type(), Some("application/json"));
    assert_eq!(
        stat.user_metadata()
            .and_then(|metadata| metadata.get("purpose").map(String::as_str)),
        Some("append")
    );
    assert_eq!(
        stat.user_metadata()
            .and_then(|metadata| metadata.get("keep").map(String::as_str)),
        None
    );

    let entries = op.list("/").await.unwrap();
    let file = entries.iter().find(|entry| entry.path() == "file").unwrap();
    assert_eq!(file.metadata().content_type(), Some("application/json"));
    assert_eq!(
        file.metadata()
            .user_metadata()
            .and_then(|metadata| metadata.get("purpose").map(String::as_str)),
        Some("append")
    );
}

#[wasm_bindgen_test]
async fn append_obeys_write_conditions() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("append_conditions_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "v1").await.unwrap();
    let etag = op.stat("file").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    let err = op
        .write_with("file", "-fail")
        .append(true)
        .if_match(&stale)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v1");

    op.write_with("file", "-ok")
        .append(true)
        .if_match(&etag)
        .await
        .unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v1-ok");

    let err = op
        .write_with("file", "-again")
        .append(true)
        .if_none_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v1-ok");

    op.write_with("absent", "created")
        .append(true)
        .if_none_match("*")
        .await
        .unwrap();
    assert_eq!(op.read("absent").await.unwrap().to_vec(), b"created");
}

#[wasm_bindgen_test]
async fn recursive_delete_removes_subtree() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("recursive_delete_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // A directory tree mixing files, nested dirs, and sibling prefixes that
    // must NOT be swept up by the recursive delete.
    op.write("dir/a", "hello").await.unwrap();
    op.write("dir/sub/b", "worlds!").await.unwrap();
    op.create_dir("dir/sub/").await.unwrap();
    op.write("dir/edge", "near").await.unwrap();
    op.write("dir-other", "sibling").await.unwrap();
    op.write("dir_other", "other sibling").await.unwrap();
    op.create_dir("keep/").await.unwrap();
    op.write("keep/x", "kept").await.unwrap();

    // Recursive delete by directory path without a trailing slash.
    op.delete_with("dir").recursive(true).await.unwrap();

    let err = op.stat("dir/a").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.stat("dir/sub/b").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.stat("dir/edge").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // Sibling prefixes that share the literal prefix string are untouched.
    assert_eq!(op.read("dir-other").await.unwrap().to_vec(), b"sibling");
    assert_eq!(
        op.read("dir_other").await.unwrap().to_vec(),
        b"other sibling"
    );
    assert_eq!(op.read("keep/x").await.unwrap().to_vec(), b"kept");

    // Recursive delete by directory path with a trailing slash.
    op.delete_with("keep/").recursive(true).await.unwrap();
    let err = op.stat("keep/x").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // Recursive delete of a single file drops only that file.
    op.write("file", "data").await.unwrap();
    op.write("file_other", "keep me").await.unwrap();
    op.delete_with("file").recursive(true).await.unwrap();
    let err = op.stat("file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op.read("file_other").await.unwrap().to_vec(), b"keep me");

    // Recursive delete of a missing path is a no-op, not an error.
    op.delete_with("does/not/exist")
        .recursive(true)
        .await
        .unwrap();

    // Non-recursive delete still works as before.
    op.write("plain", "x").await.unwrap();
    op.delete("plain").await.unwrap();
    let err = op.stat("plain").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn recursive_delete_large_subtree_keeps_sibling_prefixes() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("recursive_delete_large_range_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    for idx in 0..250 {
        op.write(format!("dir/file-{idx:03}").as_str(), "x")
            .await
            .unwrap();
    }
    op.write("dir", "exact").await.unwrap();
    op.write("dir-other/file", "keep").await.unwrap();
    op.write("dir2/file", "keep").await.unwrap();

    op.delete_with("dir").recursive(true).await.unwrap();

    let err = op.stat("dir").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.stat("dir/file-000").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.stat("dir/file-249").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op.read("dir-other/file").await.unwrap().to_vec(), b"keep");
    assert_eq!(op.read("dir2/file").await.unwrap().to_vec(), b"keep");
}

#[wasm_bindgen_test]
async fn batch_delete_removes_many_paths_including_recursive_entries() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("batch_delete_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    op.write("b", "2").await.unwrap();
    op.write("dir/x", "x").await.unwrap();
    op.write("dir/sub/y", "y").await.unwrap();
    op.write("dir-other", "keep").await.unwrap();
    op.write("keep", "keep").await.unwrap();

    op.delete_iter(vec![
        ("a".to_string(), OpDelete::new()),
        ("b".to_string(), OpDelete::new()),
        ("dir".to_string(), OpDelete::new().with_recursive(true)),
    ])
    .await
    .unwrap();

    for path in ["a", "b", "dir/x", "dir/sub/y"] {
        let err = op.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    assert_eq!(op.read("dir-other").await.unwrap().to_vec(), b"keep");
    assert_eq!(op.read("keep").await.unwrap().to_vec(), b"keep");

    // Missing entries remain no-ops when batched.
    op.delete_iter(vec!["missing".to_string(), "also-missing".to_string()])
        .await
        .unwrap();
}

#[wasm_bindgen_test]
async fn delete_iter_with_overlapping_recursive_entries_is_idempotent() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("overlapping_recursive_delete_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "a").await.unwrap();
    op.write("dir/sub/b", "b").await.unwrap();
    op.write("dir/sub/c", "c").await.unwrap();
    op.write("dir-other/keep", "keep").await.unwrap();

    op.delete_iter(vec![
        ("dir".to_string(), OpDelete::new().with_recursive(true)),
        ("dir/sub".to_string(), OpDelete::new().with_recursive(true)),
        ("dir/a".to_string(), OpDelete::new()),
    ])
    .await
    .unwrap();

    for path in ["dir/a", "dir/sub/b", "dir/sub/c"] {
        let err = op.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    assert_eq!(op.read("dir-other/keep").await.unwrap().to_vec(), b"keep");
}

#[wasm_bindgen_test]
async fn list_with_limit_caps_emitted_entries() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_limit_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // A directory with several files and a nested dir so that non-recursive
    // listing folds nothing here (all entries are direct children), while the
    // recursive list surfaces the nested file too.
    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.write("dir/c", "3").await.unwrap();
    op.write("dir/d", "4").await.unwrap();
    op.write("dir/sub/e", "5").await.unwrap();

    // limit caps the emitted entry count.
    let page = op.list_with("dir/").limit(2).await.unwrap();
    assert_eq!(page.len(), 2);

    // A limit larger than the entry set returns everything.
    let all = op.list_with("dir/").limit(100).await.unwrap();
    // a, b, c, d, sub/ (non-recursive folds `sub/` into one dir entry)
    assert_eq!(all.len(), 5);

    // Recursive listing emits every underlying key; limit still caps emitted
    // entries, not raw keys.
    let rec = op.list_with("dir/").recursive(true).limit(2).await.unwrap();
    assert_eq!(rec.len(), 2);
    let rec_all = op
        .list_with("dir/")
        .recursive(true)
        .limit(100)
        .await
        .unwrap();
    assert_eq!(rec_all.len(), 5);
    let rec_paths: Vec<String> = rec_all
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(
        rec_paths,
        vec!["dir/a", "dir/b", "dir/c", "dir/d", "dir/sub/e"]
    );
}

#[wasm_bindgen_test]
async fn delete_iter_flushes_batches_larger_than_delete_max_size() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("delete_max_size_flush_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let mut paths = Vec::new();
    for i in 0..105 {
        let path = format!("batch/file-{i:03}");
        op.write(&path, "x").await.unwrap();
        paths.push(path);
    }

    op.delete_iter(paths.clone()).await.unwrap();

    for path in paths {
        let err = op.stat(&path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
}

#[wasm_bindgen_test]
async fn list_with_start_after_resumes() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_start_after_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.write("dir/c", "3").await.unwrap();
    op.write("dir/d", "4").await.unwrap();
    op.write("dir/e", "5").await.unwrap();

    // start_after excludes the named entry and everything before it.
    let tail = op.list_with("dir/").start_after("dir/b").await.unwrap();
    let names: Vec<String> = tail.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/c", "dir/d", "dir/e"]);

    // start_after past the last entry yields an empty page.
    let empty = op.list_with("dir/").start_after("dir/e").await.unwrap();
    assert!(empty.is_empty());

    // Paginating with limit + start_after walks the whole set in order.
    let mut collected = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut builder = op.list_with("dir/").limit(2);
        if let Some(a) = after {
            builder = builder.start_after(a.as_str());
        }
        let page = builder.await.unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|e| e.path().to_string());
        collected.extend(page.iter().map(|e| e.path().to_string()));
    }
    assert_eq!(collected, vec!["dir/a", "dir/b", "dir/c", "dir/d", "dir/e"]);
}

#[wasm_bindgen_test]
async fn list_with_start_after_skips_entries_up_to_it() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_start_after_lex_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // start_after is an absolute-key resume point: every key that sorts at or
    // before it is skipped, and listing continues from the next key inside the
    // listed prefix. The store is keyed by absolute path, so the comparison is
    // lexicographic over those keys.
    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.write("dir/c", "3").await.unwrap();

    let tail = op.list_with("dir/").start_after("dir/a").await.unwrap();
    let names: Vec<String> = tail.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/b", "dir/c"]);

    // start_after equal to the last entry yields nothing.
    let none = op.list_with("dir/").start_after("dir/c").await.unwrap();
    assert!(none.is_empty());
}

#[wasm_bindgen_test]
async fn list_with_start_after_skips_folded_directory_children() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_start_after_folded_dir_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/sub/b", "2").await.unwrap();
    op.write("dir/sub/c", "3").await.unwrap();
    op.write("dir/z", "4").await.unwrap();

    let first_page = op.list_with("dir/").limit(2).await.unwrap();
    let first_names: Vec<String> = first_page
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(first_names, vec!["dir/a", "dir/sub/"]);

    let second_page = op
        .list_with("dir/")
        .start_after("dir/sub/")
        .limit(2)
        .await
        .unwrap();
    let second_names: Vec<String> = second_page
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(second_names, vec!["dir/z"]);
}

#[wasm_bindgen_test]
async fn list_limit_with_large_folded_dir_resumes_after_directory() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_limit_large_folded_dir_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    for idx in 0..100 {
        let path = format!("dir/sub/file-{idx:03}");
        op.write(path.as_str(), "x").await.unwrap();
    }
    op.write("dir/z", "2").await.unwrap();

    let first_page = op.list_with("dir/").limit(2).await.unwrap();
    let first_names: Vec<String> = first_page
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(first_names, vec!["dir/a", "dir/sub/"]);

    let second_page = op
        .list_with("dir/")
        .start_after("dir/sub/")
        .limit(1)
        .await
        .unwrap();
    let second_names: Vec<String> = second_page
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(second_names, vec!["dir/z"]);
}

#[wasm_bindgen_test]
async fn list_start_after_outside_prefix_respects_prefix_bounds() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_start_after_prefix_bounds_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.write("other/a", "3").await.unwrap();

    let all = op.list_with("dir/").start_after("abc").await.unwrap();
    let all_names: Vec<String> = all.iter().map(|entry| entry.path().to_string()).collect();
    assert_eq!(all_names, vec!["dir/a", "dir/b"]);

    let empty = op.list_with("dir/").start_after("dir0").await.unwrap();
    assert!(empty.is_empty());
}

#[wasm_bindgen_test]
async fn etag_is_stable_and_differs_by_content() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("etag_stable_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // Same content → same ETag (across rewrites and copy).
    op.write("a", "hello").await.unwrap();
    let etag_a = op.stat("a").await.unwrap().etag().unwrap().to_string();
    assert!(etag_a.starts_with('"') && etag_a.ends_with('"'));
    assert_eq!(etag_a, format!("\"{}\"", blake3::hash(b"hello").to_hex()));

    op.write("b", "hello").await.unwrap();
    let etag_b = op.stat("b").await.unwrap().etag().unwrap().to_string();
    assert_eq!(etag_a, etag_b);

    // Rewrite same content → same ETag.
    op.write("a", "hello").await.unwrap();
    assert_eq!(op.stat("a").await.unwrap().etag().unwrap(), etag_a);

    // Different content → different ETag.
    op.write("c", "world").await.unwrap();
    let etag_c = op.stat("c").await.unwrap().etag().unwrap().to_string();
    assert_ne!(etag_a, etag_c);

    // Copy preserves the ETag (same bytes, like S3 single-part copy).
    op.copy("a", "d").await.unwrap();
    assert_eq!(op.stat("d").await.unwrap().etag().unwrap(), etag_a);

    // Rename preserves the ETag.
    op.rename("b", "e").await.unwrap();
    assert_eq!(op.stat("e").await.unwrap().etag().unwrap(), etag_a);

    // stat and list agree, and read returns the same ETag.
    let reader = op.reader_with("a").await.unwrap();
    reader.read(0..).await.unwrap();
    assert_eq!(reader.metadata().unwrap().etag().unwrap(), etag_a);
    let entries = op.list("/").await.unwrap();
    let a = entries.iter().find(|e| e.path() == "a").unwrap();
    assert_eq!(a.metadata().etag().unwrap(), etag_a);
}

#[wasm_bindgen_test]
async fn stat_read_with_if_match_and_if_none_match() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("if_match_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "hello").await.unwrap();
    let etag = op.stat("file").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    // stat: matching if_match returns the entry; a stale one fails.
    op.stat_with("file").if_match(&etag).await.unwrap();
    let err = op.stat_with("file").if_match(&stale).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // if_match "*" matches any existing entry.
    op.stat_with("file").if_match("*").await.unwrap();
    // but not a missing one.
    let err = op.stat_with("missing").if_match("*").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // stat: if_none_match fails when it matches, passes when it differs.
    let err = op.stat_with("file").if_none_match(&etag).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.stat_with("file").if_none_match(&stale).await.unwrap();

    // if_none_match "*" fails for any existing entry.
    let err = op.stat_with("file").if_none_match("*").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // read mirrors stat's condition semantics.
    op.read_with("file").if_match(&etag).await.unwrap();
    let err = op.read_with("file").if_match(&stale).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    let err = op.read_with("file").if_none_match(&etag).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.read_with("file").if_none_match(&stale).await.unwrap();
}

#[wasm_bindgen_test]
async fn read_if_match_and_if_none_match_wildcard() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("read_wildcard_conditions_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "hello").await.unwrap();

    assert_eq!(
        op.read_with("file").if_match("*").await.unwrap().to_vec(),
        b"hello"
    );

    let err = op.read_with("file").if_none_match("*").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    let err = op.read_with("missing").if_match("*").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    let err = op
        .read_with("missing")
        .if_none_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn write_with_if_match_and_if_none_match() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("write_if_match_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "v1").await.unwrap();
    let etag = op.stat("file").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    // if_match with the current ETag writes; the new content gets a new ETag.
    op.write_with("file", "v2").if_match(&etag).await.unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v2");
    let new_etag = op.stat("file").await.unwrap().etag().unwrap().to_string();
    assert_ne!(etag, new_etag);

    // if_match with the now-stale ETag fails, leaving the content untouched.
    let err = op
        .write_with("file", "v3")
        .if_match(&etag)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v2");

    // if_match "*" requires the entry to exist; here it does, so it writes.
    op.write_with("file", "v4").if_match("*").await.unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v4");

    // if_match "*" on a missing path fails (the precondition can't hold).
    let err = op
        .write_with("missing", "x")
        .if_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // if_none_match writes when the ETag differs.
    op.write_with("file", "v5")
        .if_none_match(&stale)
        .await
        .unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v5");

    // if_none_match with the current ETag fails.
    let cur = op.stat("file").await.unwrap().etag().unwrap().to_string();
    let err = op
        .write_with("file", "v6")
        .if_none_match(&cur)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"v5");

    // if_none_match "*" acts like if_not_exists: it writes only when absent.
    let err = op
        .write_with("file", "v7")
        .if_none_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.write_with("fresh", "v8")
        .if_none_match("*")
        .await
        .unwrap();
    assert_eq!(op.read("fresh").await.unwrap().to_vec(), b"v8");
}

#[wasm_bindgen_test]
async fn copy_with_if_match() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("copy_if_match_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("src", "hello").await.unwrap();
    op.write("dst", "old").await.unwrap();
    let dst_etag = op.stat("dst").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    // copy succeeds when the existing destination ETag matches.
    op.copy_with("src", "dst")
        .if_match(&dst_etag)
        .await
        .unwrap();
    assert_eq!(op.read("dst").await.unwrap().to_vec(), b"hello");

    // copy fails when the destination ETag doesn't match; dst2 is left untouched.
    op.write("dst2", "old2").await.unwrap();
    let err = op
        .copy_with("src", "dst2")
        .if_match(&stale)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("dst2").await.unwrap().to_vec(), b"old2");

    // copy "*" matches any existing destination.
    op.write("dst3", "old3").await.unwrap();
    op.copy_with("src", "dst3").if_match("*").await.unwrap();
    assert_eq!(op.read("dst3").await.unwrap().to_vec(), b"hello");

    // copy "*" requires the destination to exist.
    let err = op
        .copy_with("src", "missing-dst")
        .if_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // copy from a missing source is NotFound before destination conditions.
    let err = op
        .copy_with("missing", "dst4")
        .if_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn copy_with_if_match_uses_legacy_destination_etag_fallback() {
    let db_name = fresh_test_db_name("copy_legacy_destination_if_match_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    #[derive(serde::Serialize)]
    struct Record<'a> {
        k: &'a str,
        #[serde(with = "serde_bytes")]
        v: &'a [u8],
    }
    #[derive(serde::Serialize)]
    struct LegacyMeta<'a> {
        k: &'a str,
        l: f64,
        m: f64,
    }

    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            evt.database()
                .build_object_store("__opendal_indexeddb_meta__main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main", "__opendal_indexeddb_meta__main"])
        .rw()
        .run(|txn| async move {
            let src = serde_wasm_bindgen::to_value(&Record {
                k: "src",
                v: b"source",
            })
            .unwrap();
            let dst = serde_wasm_bindgen::to_value(&Record {
                k: "dst",
                v: b"legacy-dst",
            })
            .unwrap();
            let dst_meta = serde_wasm_bindgen::to_value(&LegacyMeta {
                k: "dst",
                l: 10.0,
                m: js_sys::Date::now(),
            })
            .unwrap();

            txn.object_store("main")?.put(&src).await?;
            txn.object_store("main")?.put(&dst).await?;
            txn.object_store("__opendal_indexeddb_meta__main")?
                .put(&dst_meta)
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let dst_etag = op.stat("dst").await.unwrap().etag().unwrap().to_string();
    op.copy_with("src", "dst")
        .if_match(&dst_etag)
        .await
        .unwrap();

    assert_eq!(op.read("dst").await.unwrap().to_vec(), b"source");
    assert_eq!(op.stat("dst").await.unwrap().content_length(), 6);
}

#[wasm_bindgen_test]
async fn stat_with_if_modified_since_and_unmodified_since() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("stat_modified_since_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "hello").await.unwrap();
    let last_modified = op.stat("file").await.unwrap().last_modified().unwrap();

    // A second before the entry's mtime: it "has been modified since" →
    // if_modified_since is satisfied (entry returned), if_unmodified_since fails.
    let before = last_modified - Duration::from_secs(60);
    let stat = op
        .stat_with("file")
        .if_modified_since(before)
        .await
        .unwrap();
    assert_eq!(stat.content_length(), 5);

    let err = op
        .stat_with("file")
        .if_unmodified_since(before)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // A second after the entry's mtime: it "has not been modified since" →
    // if_modified_since fails, if_unmodified_since is satisfied.
    let after = last_modified + Duration::from_secs(60);
    let err = op
        .stat_with("file")
        .if_modified_since(after)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    op.stat_with("file")
        .if_unmodified_since(after)
        .await
        .unwrap();

    // The exact mtime is "not modified since" (the comparison is exclusive on
    // the modified side): if_modified_since at the mtime fails.
    let err = op
        .stat_with("file")
        .if_modified_since(last_modified)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.stat_with("file")
        .if_unmodified_since(last_modified)
        .await
        .unwrap();

    // A missing path is reported as NotFound, not as a condition mismatch.
    let err = op
        .stat_with("missing")
        .if_modified_since(before)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn read_with_if_modified_since_and_unmodified_since() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("read_modified_since_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "hello").await.unwrap();
    let last_modified = op.stat("file").await.unwrap().last_modified().unwrap();
    let before = last_modified - Duration::from_secs(60);
    let after = last_modified + Duration::from_secs(60);

    // if_modified_since satisfied: the read returns the content.
    let bs = op
        .read_with("file")
        .if_modified_since(before)
        .await
        .unwrap();
    assert_eq!(bs.to_vec(), b"hello");

    // if_unmodified_since violated: the condition fails before reading.
    let err = op
        .read_with("file")
        .if_unmodified_since(before)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // if_modified_since violated.
    let err = op
        .read_with("file")
        .if_modified_since(after)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    // if_unmodified_since satisfied.
    let bs = op
        .read_with("file")
        .if_unmodified_since(after)
        .await
        .unwrap();
    assert_eq!(bs.to_vec(), b"hello");

    // A missing path is NotFound.
    let err = op
        .read_with("missing")
        .if_modified_since(before)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

fn assert_content_metadata(meta: &Metadata) {
    assert_eq!(meta.content_type(), Some("text/plain"));
    assert_eq!(
        meta.user_metadata()
            .and_then(|metadata| metadata.get("owner").map(String::as_str)),
        Some("opendal")
    );
    assert_eq!(
        meta.user_metadata()
            .and_then(|metadata| metadata.get("purpose").map(String::as_str)),
        Some("metadata")
    );
}

#[wasm_bindgen_test]
async fn write_carries_content_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("write_metadata_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let written = op
        .write_with("file", "hello")
        .content_type("text/plain")
        .user_metadata([
            ("owner".to_string(), "opendal".to_string()),
            ("purpose".to_string(), "metadata".to_string()),
        ])
        .await
        .unwrap();
    assert_content_metadata(&written);

    let stat = op.stat("file").await.unwrap();
    assert_content_metadata(&stat);

    let entries = op.list("/").await.unwrap();
    let file = entries.iter().find(|entry| entry.path() == "file").unwrap();
    assert_content_metadata(file.metadata());

    op.copy("file", "copy").await.unwrap();
    let copied = op.stat("copy").await.unwrap();
    assert_content_metadata(&copied);

    op.rename("copy", "moved").await.unwrap();
    let moved = op.stat("moved").await.unwrap();
    assert_content_metadata(&moved);

    op.write("file", "plain").await.unwrap();
    let overwritten = op.stat("file").await.unwrap();
    assert_eq!(overwritten.content_type(), None);
    assert_eq!(overwritten.user_metadata(), None);
}

#[wasm_bindgen_test]
async fn malformed_meta_falls_back_to_data_store() {
    let db_name = fresh_test_db_name("malformed_meta_fallback_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    #[derive(serde::Serialize)]
    struct Record<'a> {
        k: &'a str,
        #[serde(with = "serde_bytes")]
        v: &'a [u8],
    }
    #[derive(serde::Serialize)]
    struct MalformedMeta<'a> {
        k: &'a str,
        l: &'a str,
        m: &'a str,
    }

    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            evt.database()
                .build_object_store("__opendal_indexeddb_meta__main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main", "__opendal_indexeddb_meta__main"])
        .rw()
        .run(|txn| async move {
            let record = serde_wasm_bindgen::to_value(&Record {
                k: "file",
                v: b"hello",
            })
            .unwrap();
            let malformed_meta = serde_wasm_bindgen::to_value(&MalformedMeta {
                k: "file",
                l: "not-a-number",
                m: "not-a-timestamp",
            })
            .unwrap();

            txn.object_store("main")?.put(&record).await?;
            txn.object_store("__opendal_indexeddb_meta__main")?
                .put(&malformed_meta)
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let meta = op.stat("file").await.unwrap();
    let etag = meta.etag().unwrap().to_string();
    assert_eq!(meta.content_length(), 5);
    assert_eq!(etag, format!("\"{}\"", blake3::hash(b"hello").to_hex()));
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"hello");

    let entries = op.list("/").await.unwrap();
    let file = entries.iter().find(|entry| entry.path() == "file").unwrap();
    assert_eq!(file.metadata().content_length(), 5);
    assert_eq!(file.metadata().etag(), Some(etag.as_str()));

    op.write_with("file", "world")
        .if_match(&etag)
        .await
        .unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"world");
}

#[wasm_bindgen_test]
async fn malformed_data_record_value_is_rejected() {
    let db_name = fresh_test_db_name("malformed_data_record_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            evt.database()
                .build_object_store("__opendal_indexeddb_meta__main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main", "__opendal_indexeddb_meta__main"])
        .rw()
        .run(|txn| async move {
            let bad_record = js_sys::Object::new();
            js_sys::Reflect::set(
                &bad_record,
                &wasm_bindgen::JsValue::from_str("k"),
                &wasm_bindgen::JsValue::from_str("bad"),
            )
            .unwrap();
            let missing_value_record = js_sys::Object::new();
            js_sys::Reflect::set(
                &missing_value_record,
                &wasm_bindgen::JsValue::from_str("k"),
                &wasm_bindgen::JsValue::from_str("missing-v"),
            )
            .unwrap();

            js_sys::Reflect::set(
                &bad_record,
                &wasm_bindgen::JsValue::from_str("v"),
                &wasm_bindgen::JsValue::from_str("not-bytes"),
            )
            .unwrap();

            txn.object_store("main")?.put(&bad_record).await?;
            txn.object_store("main")?.put(&missing_value_record).await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let err = op.read("bad").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);

    let err = op.read("missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);

    let err = op
        .write_with("bad", "append")
        .append(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);

    let err = op
        .write_with("missing-v", "append")
        .append(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);

    let err = op.copy("bad", "copy").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    let err = op.stat("copy").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    let err = op.copy("missing-v", "copy-missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    let err = op.stat("copy-missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    let err = op.rename("bad", "moved").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    let err = op.stat("moved").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    let err = op.rename("missing-v", "moved-missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    let err = op.stat("moved-missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    // The source record remains present but unreadable, so a later repair or
    // explicit delete can still address it by path.
    assert!(op.exists("bad").await.unwrap());
    assert!(op.exists("missing-v").await.unwrap());
    let err = op.read("bad").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
    let err = op.read("missing-v").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unexpected);
}

#[wasm_bindgen_test]
async fn orphan_meta_records_are_ignored_and_do_not_block_recreate() {
    let db_name = fresh_test_db_name("orphan_meta_ignored_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    #[derive(serde::Serialize)]
    struct Meta<'a> {
        k: &'a str,
        l: f64,
        m: f64,
        e: &'a str,
    }

    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            evt.database()
                .build_object_store("__opendal_indexeddb_meta__main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main", "__opendal_indexeddb_meta__main"])
        .rw()
        .run(|txn| async move {
            let orphan = serde_wasm_bindgen::to_value(&Meta {
                k: "ghost",
                l: 5.0,
                m: js_sys::Date::now(),
                e: "\"orphan-etag\"",
            })
            .unwrap();

            txn.object_store("__opendal_indexeddb_meta__main")?
                .put(&orphan)
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let err = op.stat("ghost").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.read("ghost").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(op.list("/").await.unwrap().is_empty());

    op.write_with("ghost", "created")
        .if_not_exists(true)
        .await
        .unwrap();
    assert_eq!(op.read("ghost").await.unwrap().to_vec(), b"created");
    assert_eq!(op.stat("ghost").await.unwrap().content_length(), 7);
}

#[wasm_bindgen_test]
async fn old_format_without_meta_store_is_compatible() {
    let db_name = fresh_test_db_name("compat_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    // Simulate a database written by an old version of this crate:
    // only the data store exists, records carry no sidecar metadata.
    #[derive(serde::Serialize)]
    struct OldRecord<'a> {
        k: &'a str,
        #[serde(with = "serde_bytes")]
        v: &'a [u8],
    }
    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main"])
        .rw()
        .run(|txn| async move {
            let record = serde_wasm_bindgen::to_value(&OldRecord {
                k: "old",
                v: b"hello",
            })
            .unwrap();
            txn.object_store("main")?.put(&record).await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // stat falls back to measuring the stored value
    let meta = op.stat("old").await.unwrap();
    let etag = meta.etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();
    assert_eq!(meta.content_length(), 5);
    assert_eq!(etag, format!("\"{}\"", blake3::hash(b"hello").to_hex()));

    // conditional stat/read also use the lazily computed ETag for data-only
    // legacy records.
    let meta = op.stat_with("old").if_match(&etag).await.unwrap();
    assert_eq!(meta.content_length(), 5);
    let err = op.stat_with("old").if_match(&stale).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    let err = op.stat_with("old").if_none_match(&etag).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    op.stat_with("old").if_none_match(&stale).await.unwrap();

    assert_eq!(
        op.read_with("old").if_match(&etag).await.unwrap().to_vec(),
        b"hello"
    );
    let err = op.read_with("old").if_match(&stale).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    let err = op.read_with("old").if_none_match(&etag).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(
        op.read_with("old")
            .if_none_match(&stale)
            .await
            .unwrap()
            .to_vec(),
        b"hello"
    );

    // list carries the length measured from the old record
    let entries = op.list("/").await.unwrap();
    let old = entries.iter().find(|e| e.path() == "old").unwrap();
    assert_eq!(old.metadata().content_length(), 5);

    let limited = op.list_with("/").limit(1).await.unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].path(), "old");
    assert_eq!(limited[0].metadata().content_length(), 5);
    assert!(limited[0].metadata().etag().is_some());

    // copy and rename synthesize sidecar metadata for old records
    op.copy("old", "old_copy").await.unwrap();
    assert_eq!(op.stat("old_copy").await.unwrap().content_length(), 5);
    op.rename("old_copy", "old_moved").await.unwrap();
    assert_eq!(op.stat("old_moved").await.unwrap().content_length(), 5);
    assert_eq!(op.read("old_moved").await.unwrap().to_vec(), b"hello");

    // reads still work, and rewriting upgrades the record with sidecar metadata
    assert_eq!(op.read("old").await.unwrap().to_vec(), b"hello");
    op.write("old", "worlds!").await.unwrap();
    let meta = op.stat("old").await.unwrap();
    assert_eq!(meta.content_length(), 7);
    assert!(meta.last_modified().is_some());

    // delete removes the entry from both stores
    op.delete("old").await.unwrap();
    let err = op.stat("old").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn append_to_legacy_record_without_meta_store() {
    let db_name = fresh_test_db_name("append_legacy_data_only_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    #[derive(serde::Serialize)]
    struct OldRecord<'a> {
        k: &'a str,
        #[serde(with = "serde_bytes")]
        v: &'a [u8],
    }
    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main"])
        .rw()
        .run(|txn| async move {
            let record = serde_wasm_bindgen::to_value(&OldRecord {
                k: "old",
                v: b"hello",
            })
            .unwrap();
            txn.object_store("main")?.put(&record).await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let appended = op.write_with("old", " world").append(true).await.unwrap();
    assert_eq!(appended.content_length(), 11);
    assert!(appended.etag().is_some());
    assert_eq!(op.read("old").await.unwrap().to_vec(), b"hello world");

    let meta = op.stat("old").await.unwrap();
    let etag = meta.etag().unwrap().to_string();
    assert_eq!(meta.content_length(), 11);
    assert_eq!(meta.etag(), appended.etag());

    op.write_with("old", "!")
        .append(true)
        .if_match(&etag)
        .await
        .unwrap();
    assert_eq!(op.read("old").await.unwrap().to_vec(), b"hello world!");
    assert_eq!(op.stat("old").await.unwrap().content_length(), 12);
}

#[wasm_bindgen_test]
async fn legacy_entries_without_mtime_pass_if_modified_since() {
    let db_name = fresh_test_db_name("compat_modified_since_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();

    // Simulate a database written by an old version: data store only, no
    // sidecar meta, therefore the legacy entry has no last-modified timestamp.
    #[derive(serde::Serialize)]
    struct OldRecord<'a> {
        k: &'a str,
        #[serde(with = "serde_bytes")]
        v: &'a [u8],
    }
    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database()
                .build_object_store("main")
                .key_path("k")
                .create()?;
            Ok(())
        })
        .await
        .unwrap();
    db.transaction(&["main"])
        .rw()
        .run(|txn| async move {
            let record = serde_wasm_bindgen::to_value(&OldRecord {
                k: "legacy",
                v: b"hello",
            })
            .unwrap();
            txn.object_store("main")?.put(&record).await?;
            Ok(())
        })
        .await
        .unwrap();
    db.close();

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // Legacy entries have no mtime, so we cannot disprove that they were
    // modified — both if_modified_since and if_unmodified_since must succeed.
    let epoch = opendal::raw::Timestamp::from_second(0).unwrap();
    let distant_future = opendal::raw::Timestamp::from_second(9999999999i64).unwrap();

    let meta = op
        .stat_with("legacy")
        .if_modified_since(epoch)
        .await
        .unwrap();
    assert_eq!(meta.content_length(), 5);

    let meta = op
        .stat_with("legacy")
        .if_modified_since(distant_future)
        .await
        .unwrap();
    assert_eq!(meta.content_length(), 5);

    let meta = op
        .stat_with("legacy")
        .if_unmodified_since(epoch)
        .await
        .unwrap();
    assert_eq!(meta.content_length(), 5);

    let bs = op
        .read_with("legacy")
        .if_modified_since(epoch)
        .await
        .unwrap();
    assert_eq!(bs.to_vec(), b"hello");

    let bs = op
        .read_with("legacy")
        .if_modified_since(distant_future)
        .await
        .unwrap();
    assert_eq!(bs.to_vec(), b"hello");

    let bs = op
        .read_with("legacy")
        .if_unmodified_since(epoch)
        .await
        .unwrap();
    assert_eq!(bs.to_vec(), b"hello");
}

#[wasm_bindgen_test]
async fn root_scopes_all_operations() {
    let db_name = fresh_test_db_name("root_scope_test").await;
    let object_store_name = Some("main".to_string());
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: Some("/tenant-a/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_b = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name,
            root: Some("/tenant-b/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op_a.write("dir/a", "a").await.unwrap();
    op_a.write("dir/b", "b").await.unwrap();
    op_a.write("dir/sub/c", "c").await.unwrap();
    op_b.write("dir/a", "other").await.unwrap();

    assert_eq!(op_a.read("dir/a").await.unwrap().to_vec(), b"a");
    assert_eq!(op_b.read("dir/a").await.unwrap().to_vec(), b"other");

    let a_entries = op_a.list("dir/").await.unwrap();
    let a_names: Vec<String> = a_entries
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(a_names, vec!["dir/a", "dir/b", "dir/sub/"]);

    let a_tail = op_a.list_with("dir/").start_after("dir/a").await.unwrap();
    let a_tail_names: Vec<String> = a_tail
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(a_tail_names, vec!["dir/b", "dir/sub/"]);

    op_a.copy("dir/a", "copy").await.unwrap();
    assert_eq!(op_a.read("copy").await.unwrap().to_vec(), b"a");
    let err = op_b.stat("copy").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    op_a.rename("copy", "moved").await.unwrap();
    assert_eq!(op_a.read("moved").await.unwrap().to_vec(), b"a");
    let err = op_a.stat("copy").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    op_a.delete_with("dir").recursive(true).await.unwrap();
    for path in ["dir/a", "dir/b", "dir/sub/c"] {
        let err = op_a.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    assert_eq!(op_a.read("moved").await.unwrap().to_vec(), b"a");
    assert_eq!(op_b.read("dir/a").await.unwrap().to_vec(), b"other");
}

#[wasm_bindgen_test]
async fn builder_setters_configure_operator() {
    let db_name = fresh_test_db_name("builder_setters_test").await;
    let store = "setter_store";
    let root = "/sub";

    let op = Operator::new(
        IndexeddbBuilder::default()
            .db_name(&db_name)
            .object_store_name(store)
            .root(root),
    )
    .unwrap()
    .finish();
    op.write("file", "configured").await.unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"configured");

    let same = Operator::new(
        IndexeddbBuilder::default()
            .db_name(&db_name)
            .object_store_name(store)
            .root(root),
    )
    .unwrap()
    .finish();
    assert_eq!(same.read("file").await.unwrap().to_vec(), b"configured");

    let unscoped = Operator::new(
        IndexeddbBuilder::default()
            .db_name(&db_name)
            .object_store_name(store),
    )
    .unwrap()
    .finish();
    let err = unscoped.stat("file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(
        unscoped.read("sub/file").await.unwrap().to_vec(),
        b"configured"
    );
}

#[wasm_bindgen_test]
async fn recursive_delete_root_removes_only_current_root() {
    let db_name = fresh_test_db_name("recursive_delete_root_test").await;
    let object_store_name = Some("main".to_string());
    let op_root = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_tenant = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name,
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op_root.write("a", "1").await.unwrap();
    op_root.write("dir/b", "2").await.unwrap();
    op_tenant.write("a", "tenant").await.unwrap();

    op_root.delete_with("/").recursive(true).await.unwrap();

    for path in ["a", "dir/b"] {
        let err = op_root.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    let err = op_tenant.stat("a").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    op_tenant.write("a", "tenant").await.unwrap();
    op_tenant.write("dir/b", "nested").await.unwrap();
    op_root.write("outside", "kept").await.unwrap();

    op_tenant.delete_with("/").recursive(true).await.unwrap();

    for path in ["a", "dir/b"] {
        let err = op_tenant.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    assert_eq!(op_root.read("outside").await.unwrap().to_vec(), b"kept");
}

#[wasm_bindgen_test]
async fn reserved_meta_store_prefix_is_rejected() {
    let result = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("reserved_meta_prefix_test").await),
            object_store_name: Some("__opendal_indexeddb_meta__tenant".to_string()),
            root: None,
        }
        .into_builder(),
    );

    match result {
        Ok(_) => panic!("reserved object store prefix should be rejected"),
        Err(err) => assert_eq!(err.kind(), ErrorKind::ConfigInvalid),
    }
}

#[wasm_bindgen_test]
async fn empty_db_name_and_store_name_are_rejected() {
    let result = Operator::new(
        IndexeddbConfig {
            db_name: Some(String::new()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    );
    match result {
        Ok(_) => panic!("empty db_name should be rejected"),
        Err(err) => assert_eq!(err.kind(), ErrorKind::ConfigInvalid),
    }

    let result = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("empty_store_name_test").await),
            object_store_name: Some(String::new()),
            root: None,
        }
        .into_builder(),
    );
    match result {
        Ok(_) => panic!("empty object_store_name should be rejected"),
        Err(err) => assert_eq!(err.kind(), ErrorKind::ConfigInvalid),
    }
}

#[wasm_bindgen_test]
async fn adjacent_store_names_do_not_collide() {
    let db_name = fresh_test_db_name("adjacent_store_names_test").await;
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("store".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_b = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: Some("store_meta".to_string()),
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op_a.write("same", "from-a").await.unwrap();
    op_b.write("same", "from-b").await.unwrap();

    assert_eq!(op_a.read("same").await.unwrap().to_vec(), b"from-a");
    assert_eq!(op_b.read("same").await.unwrap().to_vec(), b"from-b");

    op_a.delete("same").await.unwrap();

    let err = op_a.stat("same").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op_b.read("same").await.unwrap().to_vec(), b"from-b");
}

#[wasm_bindgen_test]
async fn failed_copy_conditions_are_atomic() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("copy_condition_atomic_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("src", "source").await.unwrap();
    op.write("dst", "destination").await.unwrap();
    let dst_etag = op.stat("dst").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    let err = op
        .copy_with("src", "dst")
        .if_not_exists(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("dst").await.unwrap().to_vec(), b"destination");
    assert_eq!(
        op.stat("dst").await.unwrap().etag(),
        Some(dst_etag.as_str())
    );

    let err = op
        .copy_with("src", "dst")
        .if_match(&stale)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("dst").await.unwrap().to_vec(), b"destination");
    assert_eq!(
        op.stat("dst").await.unwrap().etag(),
        Some(dst_etag.as_str())
    );
}

#[wasm_bindgen_test]
async fn failed_write_conditions_preserve_empty_object() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("write_empty_condition_atomic_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("empty", Vec::<u8>::new()).await.unwrap();
    let empty_etag = op.stat("empty").await.unwrap().etag().unwrap().to_string();
    let stale = "\"0000000000000000\"".to_string();

    let err = op
        .write_with("empty", "new")
        .if_match(&stale)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);

    let meta = op.stat("empty").await.unwrap();
    assert_eq!(meta.content_length(), 0);
    assert_eq!(meta.etag(), Some(empty_etag.as_str()));
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");
}

#[wasm_bindgen_test]
async fn empty_object_counts_as_existing_for_write_and_copy_conditions() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("empty_object_exists_conditions_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("empty", Vec::<u8>::new()).await.unwrap();
    op.write("src", "payload").await.unwrap();
    let empty_etag = op.stat("empty").await.unwrap().etag().unwrap().to_string();

    let err = op
        .write_with("empty", "new")
        .if_not_exists(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");
    assert_eq!(
        op.stat("empty").await.unwrap().etag(),
        Some(empty_etag.as_str())
    );

    let err = op
        .write_with("empty", "new")
        .if_none_match("*")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");
    assert_eq!(
        op.stat("empty").await.unwrap().etag(),
        Some(empty_etag.as_str())
    );

    let err = op
        .copy_with("src", "empty")
        .if_not_exists(true)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");
    assert_eq!(
        op.stat("empty").await.unwrap().etag(),
        Some(empty_etag.as_str())
    );
}

#[wasm_bindgen_test]
async fn rename_across_root_is_not_visible_to_other_roots() {
    let db_name = fresh_test_db_name("rename_root_boundary_test").await;
    let object_store_name = Some("main".to_string());
    let op_a = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: Some("/tenant-a/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_b = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name,
            root: Some("/tenant-b/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op_a.write("from", "a").await.unwrap();
    op_b.write("to", "b").await.unwrap();

    op_a.rename("from", "to").await.unwrap();

    assert_eq!(op_a.read("to").await.unwrap().to_vec(), b"a");
    assert_eq!(op_b.read("to").await.unwrap().to_vec(), b"b");
    let err = op_a.stat("from").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn non_recursive_delete_of_directory_marker_preserves_children_and_siblings() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("delete_marker_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    op.write("dir/a", "child").await.unwrap();
    op.write("dir2/a", "sibling").await.unwrap();
    op.write("dir-other", "neighbor").await.unwrap();

    op.delete("dir/").await.unwrap();

    let err = op.stat("dir/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op.read("dir/a").await.unwrap().to_vec(), b"child");
    assert_eq!(op.read("dir2/a").await.unwrap().to_vec(), b"sibling");
    assert_eq!(op.read("dir-other").await.unwrap().to_vec(), b"neighbor");
}

#[wasm_bindgen_test]
async fn recursive_delete_of_nested_prefix_does_not_cross_sibling_boundaries() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("nested_recursive_prefix_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a/b/c", "delete").await.unwrap();
    op.write("a/b/c/d", "delete-nested").await.unwrap();
    op.write("a/bc/d", "keep-bc").await.unwrap();
    op.write("a/b-c/d", "keep-b-dash").await.unwrap();
    op.write("a/b", "explicit-file").await.unwrap();

    op.delete_with("a/b").recursive(true).await.unwrap();

    for path in ["a/b", "a/b/c", "a/b/c/d"] {
        let err = op.stat(path).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }
    assert_eq!(op.read("a/bc/d").await.unwrap().to_vec(), b"keep-bc");
    assert_eq!(op.read("a/b-c/d").await.unwrap().to_vec(), b"keep-b-dash");
}

#[wasm_bindgen_test]
async fn list_empty_root_and_empty_directories_are_stable() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("empty_list_boundaries_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    assert!(op.list("/").await.unwrap().is_empty());
    assert!(op.list("missing/").await.unwrap().is_empty());

    op.create_dir("empty/").await.unwrap();

    let root = op.list("/").await.unwrap();
    let root_names: Vec<String> = root.iter().map(|entry| entry.path().to_string()).collect();
    assert_eq!(root_names, vec!["empty/"]);
    assert!(root[0].metadata().is_dir());
    assert!(op.list("empty/").await.unwrap().is_empty());
    assert!(
        op.list_with("empty/")
            .recursive(true)
            .await
            .unwrap()
            .is_empty()
    );
}

#[wasm_bindgen_test]
async fn list_limit_one_counts_folded_directory_once() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("list_limit_folded_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/sub/a", "1").await.unwrap();
    op.write("dir/sub/b", "2").await.unwrap();
    op.write("dir/z", "3").await.unwrap();

    let first = op.list_with("dir/").limit(1).await.unwrap();
    let names: Vec<String> = first.iter().map(|entry| entry.path().to_string()).collect();
    assert_eq!(names, vec!["dir/sub/"]);

    let second = op
        .list_with("dir/")
        .start_after("dir/sub/")
        .limit(1)
        .await
        .unwrap();
    let names: Vec<String> = second
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(names, vec!["dir/z"]);
}

#[wasm_bindgen_test]
async fn large_list_prefetched_metadata_is_preserved() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("large_list_meta_join_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    for idx in 0..40 {
        let path = format!("bulk/file-{idx:02}");
        let payload = vec![b'x'; idx + 1];
        op.write_with(path.as_str(), payload)
            .content_type("application/octet-stream")
            .user_metadata([("idx".to_string(), idx.to_string())])
            .await
            .unwrap();
    }

    let entries = op.list("bulk/").await.unwrap();
    assert_eq!(entries.len(), 40);

    for idx in [0usize, 31, 39] {
        let path = format!("bulk/file-{idx:02}");
        let expected_idx = idx.to_string();
        let entry = entries.iter().find(|entry| entry.path() == path).unwrap();
        let meta = entry.metadata();
        assert_eq!(meta.content_length(), (idx + 1) as u64);
        assert_eq!(meta.content_type(), Some("application/octet-stream"));
        assert!(meta.etag().is_some());
        assert_eq!(
            meta.user_metadata()
                .and_then(|metadata| metadata.get("idx").map(String::as_str)),
            Some(expected_idx.as_str())
        );
    }
}

#[wasm_bindgen_test]
async fn get_all_string_keys_reports_unfiltered_key_count() {
    let db_name = fresh_test_db_name("non_string_key_count_test").await;
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();
    let db = factory
        .open(db_name.as_str(), 1, |evt| async move {
            evt.database().build_object_store("keys").create()?;
            Ok(())
        })
        .await
        .unwrap();

    db.transaction(&["keys"])
        .rw()
        .run(|txn| async move {
            let store = txn.object_store("keys")?;
            store
                .put_kv(&JsValue::from_str("alpha"), &JsValue::from_str("alpha"))
                .await?;
            store
                .put_kv(&JsValue::from_str("beta"), &JsValue::from_str("beta"))
                .await?;
            let array_key = js_sys::Array::new();
            array_key.push(&JsValue::from_str("external"));
            let array_key: JsValue = array_key.into();
            store
                .put_kv(&array_key, &JsValue::from_str("external"))
                .await?;
            Ok(())
        })
        .await
        .unwrap();

    db.transaction(&["keys"])
        .run(|txn| async move {
            let store = txn.object_store("keys")?;
            let (keys, raw_len) =
                crate::backend::get_all_string_keys_in(&store, "", false, None, 3).await?;
            assert_eq!(keys, vec!["alpha".to_string(), "beta".to_string()]);
            assert_eq!(raw_len, 3);

            let (keys, raw_len) =
                crate::backend::get_all_string_keys_in(&store, "beta", true, None, 1).await?;
            assert!(keys.is_empty());
            assert_eq!(raw_len, 1);
            Ok(())
        })
        .await
        .unwrap();

    db.close();
}

#[wasm_bindgen_test]
async fn unbounded_list_spans_multiple_default_pages() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("unbounded_list_multi_page_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let total = 1300usize;
    for idx in 0..total {
        let path = format!("bulk/file-{idx:04}");
        let payload = vec![b'x'; idx % 13 + 1];
        op.write(path.as_str(), payload).await.unwrap();
    }

    let entries = op.list("bulk/").await.unwrap();
    assert_eq!(entries.len(), total);

    for idx in [0usize, 1023, 1024, total - 1] {
        let entry = &entries[idx];
        assert_eq!(entry.path(), format!("bulk/file-{idx:04}"));
        assert_eq!(entry.metadata().content_length(), (idx % 13 + 1) as u64);
        assert!(entry.metadata().etag().is_some());
    }
}

#[wasm_bindgen_test]
async fn read_ranges_cover_empty_and_boundary_offsets() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("read_range_extra_boundaries_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("empty", Vec::<u8>::new()).await.unwrap();
    assert_eq!(
        op.read_with("empty").range(0..).await.unwrap().to_vec(),
        b""
    );
    assert_eq!(
        op.read_with("empty").range(1..).await.unwrap().to_vec(),
        b""
    );

    op.write("data", "abcdef").await.unwrap();
    assert_eq!(op.read_with("data").range(6..).await.unwrap().to_vec(), b"");
    assert_eq!(
        op.read_with("data").range(5..6).await.unwrap().to_vec(),
        b"f"
    );
    assert_eq!(
        op.read_with("data").range(2..2).await.unwrap().to_vec(),
        b""
    );
}

#[wasm_bindgen_test]
async fn range_read_preserves_full_object_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("read_range_metadata_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let payload = (0..1024u32)
        .map(|idx| (idx % 251) as u8)
        .collect::<Vec<_>>();
    op.write("big", payload.clone()).await.unwrap();

    let reader = op.reader_with("big").await.unwrap();
    let content = reader.read(100..200).await.unwrap();
    assert_eq!(content.to_vec(), payload[100..200]);

    let meta = reader.metadata().expect("metadata must be observed");
    assert_eq!(meta.content_length(), 1024);

    let empty = reader.read(2048..).await.unwrap();
    assert_eq!(empty.to_vec(), b"");
    let meta = reader.metadata().expect("metadata must still be observed");
    assert_eq!(meta.content_length(), 1024);
}

#[wasm_bindgen_test]
async fn conditional_range_read_checks_etag_before_returning_slice() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("read_range_if_match_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "abcdef").await.unwrap();
    let etag = op.stat("file").await.unwrap().etag().unwrap().to_string();

    assert_eq!(
        op.read_with("file")
            .if_match(&etag)
            .range(1..4)
            .await
            .unwrap()
            .to_vec(),
        b"bcd"
    );

    let err = op
        .read_with("file")
        .if_match("\"0000000000000000\"")
        .range(1..4)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConditionNotMatch);
}

#[wasm_bindgen_test]
async fn binary_payload_preserves_all_byte_values() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("binary_payload_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let mut payload = Vec::new();
    for _ in 0..4 {
        payload.extend(0u8..=255);
    }

    op.write("bytes", payload.clone()).await.unwrap();

    assert_eq!(op.stat("bytes").await.unwrap().content_length(), 1024);
    assert_eq!(op.read("bytes").await.unwrap().to_vec(), payload);
    assert_eq!(
        op.read_with("bytes")
            .range(250..266)
            .await
            .unwrap()
            .to_vec(),
        (250u8..=255).chain(0u8..=9).collect::<Vec<_>>()
    );
}

#[wasm_bindgen_test]
async fn copy_and_rename_self_are_stable() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("self_copy_rename_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "same").await.unwrap();
    let before = op.stat("file").await.unwrap();
    let etag = before.etag().unwrap().to_string();

    let err = op.copy("file", "file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsSameFile);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"same");
    assert_eq!(op.stat("file").await.unwrap().etag(), Some(etag.as_str()));

    let err = op.rename("file", "file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsSameFile);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"same");
    assert_eq!(op.stat("file").await.unwrap().etag(), Some(etag.as_str()));
}

#[wasm_bindgen_test]
async fn raw_core_rename_and_copy_to_self_do_not_destroy_data() {
    let db_name = fresh_test_db_name("raw_core_self_copy_rename_test").await;
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "same").await.unwrap();

    let core = IndexeddbCore {
        db_name,
        object_store_name: "main".to_string(),
        meta_store_name: "__opendal_indexeddb_meta__main".to_string(),
    };
    let path = "/file";

    let err = core.rename(path, path).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsSameFile);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"same");

    let err = core.copy(path, path, false, None).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsSameFile);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"same");
}

#[wasm_bindgen_test]
async fn copy_and_rename_to_directory_paths_are_rejected() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("copy_rename_to_dir_path_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "data").await.unwrap();
    op.create_dir("dir/").await.unwrap();

    let err = op.copy("file", "dir/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"data");
    assert!(op.stat("dir/").await.unwrap().is_dir());

    let err = op.rename("file", "dir/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"data");
    assert!(op.stat("dir/").await.unwrap().is_dir());
}

#[wasm_bindgen_test]
async fn directory_paths_are_rejected_for_file_read_and_write() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("dir_path_file_ops_rejected_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    op.write("dir/file", "child").await.unwrap();

    let err = op.read("dir/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);

    let err = op.write("dir/", "bad").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsADirectory);

    let err = match op.writer("dir/").await {
        Ok(_) => panic!("writer for directory path should fail"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), ErrorKind::IsADirectory);

    let meta = op.stat("dir/").await.unwrap();
    assert!(meta.is_dir());
    assert_eq!(op.read("dir/file").await.unwrap().to_vec(), b"child");
}

#[wasm_bindgen_test]
async fn probe_streaming_writer_multi_chunk() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_multi_chunk_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let mut w = op.writer("multi").await.unwrap();
    w.write("hello ").await.unwrap();
    w.write("world").await.unwrap();
    let meta = w.close().await.unwrap();
    assert_eq!(meta.content_length(), 11);
    assert_eq!(op.read("multi").await.unwrap().to_vec(), b"hello world");
}

#[wasm_bindgen_test]
async fn probe_streaming_writer_empty_chunks_only() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_empty_chunks_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // Writing only empty buffers then closing should still create an empty object.
    let mut w = op.writer("empty").await.unwrap();
    w.write(Vec::<u8>::new()).await.unwrap();
    w.write(Vec::<u8>::new()).await.unwrap();
    let meta = w.close().await.unwrap();
    assert_eq!(meta.content_length(), 0);
    assert_eq!(op.read("empty").await.unwrap().to_vec(), b"");
}

#[wasm_bindgen_test]
async fn probe_writer_abort_leaves_nothing() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_abort_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let mut w = op.writer("abort").await.unwrap();
    w.write("partial").await.unwrap();
    w.abort().await.unwrap();
    let err = op.stat("abort").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn probe_create_dir_idempotent_after_content() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_create_dir_idempotent_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // create_dir twice should be a stable no-op directory marker.
    op.create_dir("dir/").await.unwrap();
    op.create_dir("dir/").await.unwrap();
    assert!(op.stat("dir/").await.unwrap().is_dir());
    assert_eq!(op.list("dir/").await.unwrap().len(), 0);
}

#[wasm_bindgen_test]
async fn probe_stat_directory_marker_does_not_carry_size() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_dir_marker_size_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    let meta = op.stat("dir/").await.unwrap();
    assert!(meta.is_dir());
    // A directory marker stores an empty buffer; it must not report a file length.
    // `Metadata::is_dir()` entries carry no content_length for non-files here.
    assert!(meta.is_dir());
    // content_length for a dir is reported as 0 or unset; ensure no real size leaks.
    assert!(meta.content_length() == 0);
}

#[wasm_bindgen_test]
async fn probe_list_root_returns_top_level_only() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_root_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    op.write("b/c", "2").await.unwrap();
    op.create_dir("d/").await.unwrap();

    let entries = op.list("/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["a", "b/", "d/"]);
}

#[wasm_bindgen_test]
async fn probe_copy_into_nested_destination_creates_markerless_parents() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_copy_nested_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("src", "hello").await.unwrap();
    op.copy("src", "deep/nested/dst").await.unwrap();
    assert_eq!(op.read("deep/nested/dst").await.unwrap().to_vec(), b"hello");
    // The intermediate "directories" are implicit; non-recursive listing of
    // "/" should fold `deep/` into a single dir entry.
    let entries = op.list("/").await.unwrap();
    assert!(entries.iter().any(|e| e.path() == "deep/"));
}

#[wasm_bindgen_test]
async fn probe_rename_overwrites_existing_file() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_rename_overwrite_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "aaaa").await.unwrap();
    op.write("b", "bbbbbb").await.unwrap();
    op.rename("a", "b").await.unwrap();
    assert_eq!(op.read("b").await.unwrap().to_vec(), b"aaaa");
    assert_eq!(op.stat("b").await.unwrap().content_length(), 4);
    assert!(op.stat("a").await.unwrap_err().kind() == ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn probe_stat_missing_is_not_found_not_unexpected() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_stat_missing_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let err = op.stat("nope").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(format!("{err:?}").contains("nope"));
}

#[wasm_bindgen_test]
async fn probe_read_missing_is_not_found() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_read_missing_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let err = op.read("nope").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn probe_delete_missing_is_noop() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_delete_missing_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.delete("nope").await.unwrap();
    op.delete_with("nope/dir").recursive(true).await.unwrap();
}

#[wasm_bindgen_test]
async fn probe_list_with_limit_zero() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_limit_zero_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    op.write("b", "2").await.unwrap();

    let entries = op.list_with("/").limit(0).await.unwrap();
    assert_eq!(entries.len(), 0);
}

#[wasm_bindgen_test]
async fn probe_list_with_start_after_and_recursive() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_start_after_recursive_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.write("dir/c", "3").await.unwrap();
    op.write("dir/sub/d", "4").await.unwrap();

    let entries = op
        .list_with("dir/")
        .recursive(true)
        .start_after("dir/b")
        .await
        .unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/c", "dir/sub/d"]);
}

#[wasm_bindgen_test]
async fn probe_overwrite_preserves_no_stale_user_metadata() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_overwrite_meta_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("file", "first")
        .user_metadata([("k".to_string(), "v".to_string())])
        .await
        .unwrap();
    assert_eq!(
        op.stat("file")
            .await
            .unwrap()
            .user_metadata()
            .and_then(|m| m.get("k").map(String::as_str)),
        Some("v")
    );

    // Plain overwrite clears user metadata.
    op.write("file", "second").await.unwrap();
    let meta = op.stat("file").await.unwrap();
    assert_eq!(meta.content_length(), 6);
    assert!(meta.user_metadata().is_none() || meta.user_metadata().unwrap().is_empty());
}

#[wasm_bindgen_test]
async fn probe_copy_preserves_content_metadata_but_updates_mtime() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_copy_meta_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("src", "hello")
        .content_type("text/plain")
        .await
        .unwrap();
    let src_mtime = op.stat("src").await.unwrap().last_modified().unwrap();

    op.copy("src", "dst").await.unwrap();
    let dst = op.stat("dst").await.unwrap();
    assert_eq!(dst.content_type(), Some("text/plain"));
    assert_eq!(dst.content_length(), 5);
    // The copy gets a fresh mtime (current time), which should be >= source mtime.
    assert!(dst.last_modified().unwrap() >= src_mtime);
}

#[wasm_bindgen_test]
async fn probe_list_file_path_prefix_returns_self() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_file_prefix_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    // OpenDAL list semantics: "If path itself exists (file or dir), it will be
    // returned as an entry in addition to any prefixed children."
    let entries = op.list("a").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["a"]);
}

#[wasm_bindgen_test]
async fn probe_list_dir_prefix_without_trailing_slash() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_dir_prefix_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    // Listing "dir" (no trailing slash) — prefix-based. The dir marker "dir/"
    // and children should be returned. Per semantics, the marker itself + kids.
    let entries = op.list("dir").await.unwrap();
    let names: Vec<String> = entries
        .iter()
        .map(|e| e.path().to_string())
        .collect::<Vec<_>>();
    // Non-recursive folding collapses dir/* into "dir/" + files under it.
    // We at least expect to see the children, not an empty list.
    assert!(
        names.iter().any(|n| n == "dir/a" || n == "dir/"),
        "got {names:?}"
    );
}

#[wasm_bindgen_test]
async fn probe_list_file_prefix_with_siblings() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_file_prefix_siblings_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    op.write("abc", "2").await.unwrap();
    op.write("abd", "3").await.unwrap();

    // list("a") is a prefix query: returns "a" (exact file) and "abc", "abd".
    let entries = op.list("a").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["a", "abc", "abd"]);
}

#[wasm_bindgen_test]
async fn probe_list_dir_with_trailing_slash_excludes_marker() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_dir_marker_excluded_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();

    // list("dir/") returns the children but NOT the "dir/" marker itself.
    let entries = op.list("dir/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/a", "dir/b"]);
}

#[wasm_bindgen_test]
async fn probe_list_recursive_includes_all_descendants() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_recursive_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/sub/b", "2").await.unwrap();
    op.write("dir/sub/deep/c", "3").await.unwrap();

    let entries = op.list_with("dir/").recursive(true).await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/a", "dir/sub/b", "dir/sub/deep/c"]);
}

#[wasm_bindgen_test]
async fn probe_reader_chunked_returns_full_content() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_reader_chunked_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let payload: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
    op.write("big", payload.clone()).await.unwrap();

    // A range read spanning the whole object returns the full content.
    assert_eq!(op.read("big").await.unwrap().to_vec(), payload);
    // A mid-object range returns exactly those bytes.
    assert_eq!(
        op.read_with("big").range(100..200).await.unwrap().to_vec(),
        payload[100..200]
    );
}

#[wasm_bindgen_test]
async fn probe_read_with_range_partial_within_bounds() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_read_range_partial_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("data", "0123456789").await.unwrap();
    assert_eq!(
        op.read_with("data").range(2..7).await.unwrap().to_vec(),
        b"23456"
    );
    assert_eq!(
        op.read_with("data").range(0..).await.unwrap().to_vec(),
        b"0123456789"
    );
    assert_eq!(
        op.read_with("data").range(5..).await.unwrap().to_vec(),
        b"56789"
    );
    assert_eq!(
        op.read_with("data").range(..=3).await.unwrap().to_vec(),
        b"0123"
    );
}

#[wasm_bindgen_test]
async fn probe_stat_root_is_dir() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_stat_root_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let meta = op.stat("/").await.unwrap();
    assert!(meta.is_dir());
}

#[wasm_bindgen_test]
async fn probe_stat_with_root_config_scoped() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_stat_root_scoped_test").await),
            object_store_name: Some("main".to_string()),
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let meta = op.stat("/").await.unwrap();
    assert!(meta.is_dir());
    // The root itself carries no content length.
    assert_eq!(meta.content_length(), 0);
}

#[wasm_bindgen_test]
async fn root_stat_ignores_conditions() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("root_stat_conditions_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let epoch = opendal::raw::Timestamp::from_second(0).unwrap();
    let future = opendal::raw::Timestamp::from_second(9999999999i64).unwrap();

    assert!(
        op.stat_with("/")
            .if_match("\"missing\"")
            .await
            .unwrap()
            .is_dir()
    );
    assert!(op.stat_with("/").if_none_match("*").await.unwrap().is_dir());
    assert!(
        op.stat_with("/")
            .if_modified_since(future)
            .await
            .unwrap()
            .is_dir()
    );
    assert!(
        op.stat_with("/")
            .if_unmodified_since(epoch)
            .await
            .unwrap()
            .is_dir()
    );

    let scoped = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("root_stat_conditions_scoped_test").await),
            object_store_name: Some("main".to_string()),
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    assert!(
        scoped
            .stat_with("/")
            .if_none_match("*")
            .await
            .unwrap()
            .is_dir()
    );
}

#[wasm_bindgen_test]
async fn probe_write_then_overwrite_with_metadata_replaces() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_overwrite_replace_meta_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("file", "first")
        .content_type("text/plain")
        .await
        .unwrap();
    assert_eq!(
        op.stat("file").await.unwrap().content_type(),
        Some("text/plain")
    );

    // Overwrite with different metadata.
    op.write_with("file", "second")
        .content_type("application/json")
        .await
        .unwrap();
    assert_eq!(
        op.stat("file").await.unwrap().content_type(),
        Some("application/json")
    );
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"second");
}

#[wasm_bindgen_test]
async fn probe_append_after_overwrite_without_metadata_keeps_old_meta() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_append_keep_meta_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write_with("file", "hello")
        .content_type("text/plain")
        .await
        .unwrap();

    // Plain overwrite drops metadata (no meta specified).
    op.write("file", "world").await.unwrap();
    let meta = op.stat("file").await.unwrap();
    assert_eq!(meta.content_type(), None);

    // Append without metadata should NOT re-introduce metadata.
    op.write_with("file", "!").append(true).await.unwrap();
    let meta = op.stat("file").await.unwrap();
    assert_eq!(meta.content_type(), None);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"world!");
}

#[wasm_bindgen_test]
async fn probe_copy_to_self_source_is_not_consumed() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_copy_self_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "data").await.unwrap();
    // copy/rename to self is rejected at the operator layer with IsSameFile.
    let err = op.copy("file", "file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::IsSameFile);
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"data");
}

#[wasm_bindgen_test]
async fn probe_list_root_with_root_config() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_root_scoped_test").await),
            object_store_name: Some("main".to_string()),
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("a", "1").await.unwrap();
    op.write("b/c", "2").await.unwrap();
    op.create_dir("d/").await.unwrap();

    let entries = op.list("/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["a", "b/", "d/"]);
}

#[wasm_bindgen_test]
async fn probe_delete_recursive_then_recreate() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_delete_recreate_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/a", "1").await.unwrap();
    op.write("dir/b", "2").await.unwrap();
    op.delete_with("dir").recursive(true).await.unwrap();
    assert!(op.stat("dir/a").await.unwrap_err().kind() == ErrorKind::NotFound);

    // Recreate after recursive delete.
    op.write("dir/a", "3").await.unwrap();
    assert_eq!(op.read("dir/a").await.unwrap().to_vec(), b"3");
}

#[wasm_bindgen_test]
async fn probe_list_file_ends_with_slash_is_empty() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_file_slash_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "1").await.unwrap();
    op.write("file_other", "2").await.unwrap();

    // RFC 3243: "list file ends with `/`" → EMPTY (the file has no children).
    let entries = op.list("file/").await.unwrap();
    assert!(entries.is_empty(), "got {entries:?}");
}

#[wasm_bindgen_test]
async fn probe_stat_file_with_trailing_slash_is_not_found() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_stat_file_slash_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "1").await.unwrap();
    // RFC 3243: "stat file with `/`" → Error NotFound.
    let err = op.stat("file/").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn probe_stat_dir_without_trailing_slash_is_not_found_or_dir() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_stat_dir_noslash_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    op.write("dir/a", "1").await.unwrap();

    // RFC 3243: "stat dir without `/`" → Error NotFound OR metadata with dir mode.
    // This backend stores the marker under `dir/`, so `dir` (no slash) is NotFound.
    let result = op.stat("dir").await;
    match result {
        Ok(meta) => assert!(meta.is_dir(), "expected dir or NotFound"),
        Err(err) => assert_eq!(err.kind(), ErrorKind::NotFound),
    }
}

#[wasm_bindgen_test]
async fn probe_list_prefix_returns_matching_children() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_prefix_match_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("abc/def_file", "1").await.unwrap();
    op.create_dir("abc/def_dir/").await.unwrap();
    op.write("abc/def_dir/xyz", "2").await.unwrap();
    op.write("abc/other", "3").await.unwrap();

    // RFC 3243 "list prefix": list("abc/def") → abc/def_file, abc/def_dir/ (folded).
    // OpenDAL does not guarantee ordering, so compare as a sorted set.
    let entries = op.list("abc/def").await.unwrap();
    let mut names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["abc/def_dir/", "abc/def_file"]);
}

#[wasm_bindgen_test]
async fn probe_list_not_exist_is_empty_not_error() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_list_missing_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    // list of a non-existent path returns empty, not an error (prefix semantics).
    let entries = op.list("nonexistent/").await.unwrap();
    assert!(entries.is_empty());
    let entries = op.list("nonexistent").await.unwrap();
    assert!(entries.is_empty());
}

#[wasm_bindgen_test]
async fn probe_is_exist_returns_false_for_missing() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_is_exist_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    assert!(!op.exists("missing").await.unwrap());
    op.write("present", "x").await.unwrap();
    assert!(op.exists("present").await.unwrap());
}

#[wasm_bindgen_test]
async fn probe_is_exist_dir_marker() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_is_exist_dir_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("dir/").await.unwrap();
    // dir marker exists.
    assert!(op.exists("dir/").await.unwrap());
}

#[wasm_bindgen_test]
async fn probe_create_dir_nested_deep() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_deep_dir_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.create_dir("a/b/c/").await.unwrap();
    assert!(op.stat("a/b/c/").await.unwrap().is_dir());
    // Intermediate implicit dirs are not stored as markers, but listing "a/"
    // folds "a/b/" into a dir entry.
    let entries = op.list("a/").await.unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|e| e.path().to_string())
            .collect::<Vec<_>>(),
        vec!["a/b/"]
    );
}

#[wasm_bindgen_test]
async fn create_dir_boundaries_keep_existing_entries() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("create_dir_boundaries_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("file", "content").await.unwrap();
    op.create_dir("file/").await.unwrap();
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"content");
    assert!(op.stat("file").await.unwrap().is_file());
    assert!(op.stat("file/").await.unwrap().is_dir());
    assert!(op.list("file/").await.unwrap().is_empty());

    op.write("parent/child", "child").await.unwrap();
    op.create_dir("parent/").await.unwrap();
    assert!(op.stat("parent/").await.unwrap().is_dir());
    assert_eq!(op.read("parent/child").await.unwrap().to_vec(), b"child");
    let entries = op.list("parent/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["parent/child"]);

    op.create_dir("/").await.unwrap();
    op.create_dir("/").await.unwrap();
    assert!(op.stat("/").await.unwrap().is_dir());
    assert_eq!(op.read("file").await.unwrap().to_vec(), b"content");
    let root_entries = op.list("/").await.unwrap();
    let root_names: Vec<String> = root_entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(root_names, vec!["file", "file/", "parent/"]);
}

#[wasm_bindgen_test]
async fn concurrent_if_none_match_writes_one_winner() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("concurrent_condition_write_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let total = 24;
    let done = Arc::new(AtomicUsize::new(0));
    let successes = Rc::new(RefCell::new(Vec::new()));
    let errors = Rc::new(RefCell::new(Vec::new()));

    for idx in 0..total {
        spawn_local({
            let op = op.clone();
            let done = done.clone();
            let successes = successes.clone();
            let errors = errors.clone();
            async move {
                let value = format!("value-{idx}");
                match op
                    .write_with("target", value.as_bytes().to_vec())
                    .if_none_match("*")
                    .await
                {
                    Ok(_) => successes.borrow_mut().push(value),
                    Err(err) => errors.borrow_mut().push(err.kind()),
                }
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    while done.load(Ordering::SeqCst) != total {
        sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(successes.borrow().len(), 1, "{:?}", successes.borrow());
    assert_eq!(errors.borrow().len(), total - 1, "{:?}", errors.borrow());
    assert!(
        errors
            .borrow()
            .iter()
            .all(|kind| *kind == ErrorKind::ConditionNotMatch),
        "{:?}",
        errors.borrow()
    );
    let content = String::from_utf8(op.read("target").await.unwrap().to_vec()).unwrap();
    assert_eq!(Some(&content), successes.borrow().first());
}

#[wasm_bindgen_test]
async fn concurrent_appends_to_same_path_preserve_all_chunks() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("concurrent_append_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let total = 24;
    let chunk_len = 8;
    op.write("target", Vec::<u8>::new()).await.unwrap();

    let done = Arc::new(AtomicUsize::new(0));
    let errors = Rc::new(RefCell::new(Vec::new()));
    for idx in 0..total {
        spawn_local({
            let op = op.clone();
            let done = done.clone();
            let errors = errors.clone();
            async move {
                let chunk = vec![idx as u8; chunk_len];
                if let Err(err) = op.write_with("target", chunk).append(true).await {
                    errors.borrow_mut().push(err.to_string());
                }
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    while done.load(Ordering::SeqCst) != total {
        sleep(Duration::from_millis(10)).await;
    }

    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    let content = op.read("target").await.unwrap().to_vec();
    assert_eq!(content.len(), total * chunk_len);
    for idx in 0..total {
        assert_eq!(
            content.iter().filter(|byte| **byte == idx as u8).count(),
            chunk_len,
            "missing or duplicated appended chunk {idx}"
        );
    }
}

#[wasm_bindgen_test]
async fn concurrent_write_delete_race_leaves_consistent_state() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("concurrent_write_delete_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("target", "initial").await.unwrap();

    let done = Arc::new(AtomicUsize::new(0));
    let errors = Rc::new(RefCell::new(Vec::new()));
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let errors = errors.clone();
        async move {
            if let Err(err) = op.write("target", "written").await {
                errors.borrow_mut().push(err.to_string());
            }
            done.fetch_add(1, Ordering::SeqCst);
        }
    });
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let errors = errors.clone();
        async move {
            if let Err(err) = op.delete("target").await {
                errors.borrow_mut().push(err.to_string());
            }
            done.fetch_add(1, Ordering::SeqCst);
        }
    });

    while done.load(Ordering::SeqCst) != 2 {
        sleep(Duration::from_millis(10)).await;
    }

    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    match op.stat("target").await {
        Ok(meta) => {
            assert_eq!(meta.content_length(), 7);
            assert_eq!(op.read("target").await.unwrap().to_vec(), b"written");
        }
        Err(err) => assert_eq!(err.kind(), ErrorKind::NotFound),
    }
}

#[wasm_bindgen_test]
async fn concurrent_copy_delete_race_is_atomic() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("concurrent_copy_delete_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("src", "source").await.unwrap();
    op.write("dst", "old").await.unwrap();

    let done = Arc::new(AtomicUsize::new(0));
    let copy_result = Rc::new(RefCell::new(None));
    let delete_result = Rc::new(RefCell::new(None));
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let copy_result = copy_result.clone();
        async move {
            let result = op
                .copy("src", "dst")
                .await
                .map(|_| ())
                .map_err(|err| err.kind());
            *copy_result.borrow_mut() = Some(result);
            done.fetch_add(1, Ordering::SeqCst);
        }
    });
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let delete_result = delete_result.clone();
        async move {
            let result = op.delete("src").await.map_err(|err| err.kind());
            *delete_result.borrow_mut() = Some(result);
            done.fetch_add(1, Ordering::SeqCst);
        }
    });

    while done.load(Ordering::SeqCst) != 2 {
        sleep(Duration::from_millis(10)).await;
    }

    assert!(delete_result.borrow().as_ref().unwrap().is_ok());
    let copy_result = *copy_result.borrow().as_ref().unwrap();
    match copy_result {
        Ok(()) => assert_eq!(op.read("dst").await.unwrap().to_vec(), b"source"),
        Err(kind) => {
            assert_eq!(kind, ErrorKind::NotFound);
            assert_eq!(op.read("dst").await.unwrap().to_vec(), b"old");
        }
    }
    let err = op.stat("src").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn concurrent_rename_list_race_returns_valid_snapshot() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("concurrent_rename_list_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("dir/from", "value").await.unwrap();
    op.write("dir/stable", "stable").await.unwrap();

    let done = Arc::new(AtomicUsize::new(0));
    let listed = Rc::new(RefCell::new(None));
    let errors = Rc::new(RefCell::new(Vec::new()));
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let errors = errors.clone();
        async move {
            if let Err(err) = op.rename("dir/from", "dir/to").await {
                errors.borrow_mut().push(err.to_string());
            }
            done.fetch_add(1, Ordering::SeqCst);
        }
    });
    spawn_local({
        let op = op.clone();
        let done = done.clone();
        let listed = listed.clone();
        let errors = errors.clone();
        async move {
            match op.list("dir/").await {
                Ok(entries) => {
                    *listed.borrow_mut() = Some(
                        entries
                            .iter()
                            .map(|entry| entry.path().to_string())
                            .collect::<Vec<_>>(),
                    );
                }
                Err(err) => errors.borrow_mut().push(err.to_string()),
            }
            done.fetch_add(1, Ordering::SeqCst);
        }
    });

    while done.load(Ordering::SeqCst) != 2 {
        sleep(Duration::from_millis(10)).await;
    }

    assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    let listed = listed.borrow().as_ref().unwrap().clone();
    assert!(listed.iter().any(|path| path == "dir/stable"), "{listed:?}");
    assert!(
        listed.iter().any(|path| path == "dir/from") ^ listed.iter().any(|path| path == "dir/to"),
        "{listed:?}"
    );

    assert_eq!(op.read("dir/to").await.unwrap().to_vec(), b"value");
    let err = op.stat("dir/from").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[wasm_bindgen_test]
async fn root_normalization_accepts_missing_and_repeated_slashes() {
    let db_name = fresh_test_db_name("root_normalization_test").await;
    let object_store_name = Some("main".to_string());
    let op_canonical = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_without_edges = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: Some("tenant".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let op_repeated = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name,
            root: Some("//tenant//".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op_canonical.write("a", "canonical").await.unwrap();
    assert_eq!(
        op_without_edges.read("a").await.unwrap().to_vec(),
        b"canonical"
    );

    op_without_edges.write("b", "without").await.unwrap();
    assert_eq!(op_canonical.read("b").await.unwrap().to_vec(), b"without");

    op_repeated.write("c", "repeated").await.unwrap();
    assert_eq!(op_canonical.read("c").await.unwrap().to_vec(), b"repeated");

    let entries = op_canonical.list("/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

#[wasm_bindgen_test]
async fn absolute_operation_paths_stay_inside_configured_root() {
    let db_name = fresh_test_db_name("absolute_path_root_test").await;
    let object_store_name = Some("main".to_string());
    let scoped = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: object_store_name.clone(),
            root: Some("/tenant/".to_string()),
        }
        .into_builder(),
    )
    .unwrap()
    .finish();
    let unscoped = Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    scoped.write("/absolute", "scoped").await.unwrap();
    assert_eq!(scoped.read("absolute").await.unwrap().to_vec(), b"scoped");
    assert_eq!(scoped.read("/absolute").await.unwrap().to_vec(), b"scoped");

    let err = unscoped.stat("absolute").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(
        unscoped.read("tenant/absolute").await.unwrap().to_vec(),
        b"scoped"
    );

    scoped.create_dir("/dir/").await.unwrap();
    scoped.write("/dir/file", "file").await.unwrap();
    let entries = scoped.list("/dir/").await.unwrap();
    let names: Vec<String> = entries.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/file"]);
}

#[wasm_bindgen_test]
async fn dot_segments_are_treated_as_regular_path_components() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("dot_segment_path_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("./file", "dot").await.unwrap();
    op.write("dir/../file", "dotdot").await.unwrap();
    op.write("dir/file", "plain").await.unwrap();

    assert_eq!(op.read("./file").await.unwrap().to_vec(), b"dot");
    assert_eq!(op.read("dir/../file").await.unwrap().to_vec(), b"dotdot");
    assert_eq!(op.read("dir/file").await.unwrap().to_vec(), b"plain");

    let root = op.list("/").await.unwrap();
    let names: Vec<String> = root.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["./", "dir/"]);

    let dir = op.list("dir/").await.unwrap();
    let names: Vec<String> = dir.iter().map(|e| e.path().to_string()).collect();
    assert_eq!(names, vec!["dir/../", "dir/file"]);
}

#[wasm_bindgen_test]
async fn unicode_prefix_list_and_recursive_delete_respect_boundaries() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("unicode_prefix_boundary_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    op.write("ÿ", "exact").await.unwrap();
    op.write("ÿ-child", "dash").await.unwrap();
    op.write("ÿ/child", "child").await.unwrap();
    op.write("ÿ/深/file", "deep").await.unwrap();
    op.write("Ā/outside", "outside").await.unwrap();
    op.write("资料", "exact-prefix").await.unwrap();
    op.write("资料/file", "delete").await.unwrap();
    op.write("资料夹/file", "keep").await.unwrap();

    let entries = op.list("ÿ").await.unwrap();
    let mut names: Vec<String> = entries
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["ÿ", "ÿ-child", "ÿ/"]);

    let entries = op.list("ÿ/").await.unwrap();
    let names: Vec<String> = entries
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(names, vec!["ÿ/child", "ÿ/深/"]);

    let tail = op.list_with("ÿ/").start_after("ÿ/child").await.unwrap();
    let names: Vec<String> = tail.iter().map(|entry| entry.path().to_string()).collect();
    assert_eq!(names, vec!["ÿ/深/"]);

    let recursive = op.list_with("ÿ/").recursive(true).await.unwrap();
    let names: Vec<String> = recursive
        .iter()
        .map(|entry| entry.path().to_string())
        .collect();
    assert_eq!(names, vec!["ÿ/child", "ÿ/深/file"]);

    op.delete_with("资料").recursive(true).await.unwrap();
    let err = op.stat("资料").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let err = op.stat("资料/file").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(op.read("资料夹/file").await.unwrap().to_vec(), b"keep");
    assert_eq!(op.read("Ā/outside").await.unwrap().to_vec(), b"outside");
    assert_eq!(op.read("ÿ-child").await.unwrap().to_vec(), b"dash");
}

#[wasm_bindgen_test]
async fn probe_concurrent_writes_to_different_paths() {
    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("probe_concurrent_different_test").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let done = Arc::new(AtomicUsize::new(0));
    for i in 0..50 {
        spawn_local({
            let op = op.clone();
            let done = done.clone();
            async move {
                let path = format!("file_{i}");
                op.write(&path, format!("v{i}")).await.unwrap();
                assert_eq!(
                    op.read(&path).await.unwrap().to_vec(),
                    format!("v{i}").into_bytes()
                );
                done.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    while done.load(Ordering::SeqCst) != 50 {
        sleep(Duration::from_millis(10)).await;
    }
}

#[wasm_bindgen_test]
#[ignore = "manual quota probe; run only in a constrained browser storage profile"]
async fn manual_quota_exhaustion_probe() {
    const CHUNK_SIZE: usize = 8 * 1024 * 1024;
    const REPORT_EVERY_BYTES: usize = 128 * 1024 * 1024;

    let op = Operator::new(
        IndexeddbConfig {
            db_name: Some(fresh_test_db_name("manual_quota_probe").await),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish();

    let payload = vec![0u8; CHUNK_SIZE];
    let mut written = 0usize;
    let mut next_report = REPORT_EVERY_BYTES;
    for i in 0usize.. {
        let path = format!("chunk-{i:06}");
        match op.write(&path, payload.clone()).await {
            Ok(_) => {
                written += CHUNK_SIZE;
                if written >= next_report {
                    console_log!("quota probe wrote {} MiB", written / 1024 / 1024);
                    next_report += REPORT_EVERY_BYTES;
                }
            }
            Err(err) => {
                console_log!(
                    "quota probe failed after {} MiB with {err:?}",
                    written / 1024 / 1024
                );
                return;
            }
        }
    }
}
