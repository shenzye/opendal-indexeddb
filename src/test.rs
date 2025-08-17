use crate::config::IndexeddbConfig;
use gloo_timers::future::sleep;
use opendal::{Configurator, Operator};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn concurrent_rw_test() {
    let session_store_config = IndexeddbConfig {
        db_name: None,
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
