use crate::config::IndexeddbConfig;
use futures::future::{join, join_all};
use opendal::raw::Access;
use opendal::{Configurator, Operator};
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

    let mut write_tasks = Vec::new();

    op.write("hello", "world").await.unwrap();
    for _ in 0..100 {
        write_tasks.push(async {
            op.write("hello", "world").await.unwrap();
        });
    }

    let mut read_tasks = Vec::new();
    for _ in 0..1000 {
        read_tasks.push(async {
            assert_eq!(op.read("hello").await.unwrap().to_vec(), b"world");
        });
    }

    join(join_all(write_tasks), join_all(read_tasks)).await;
}
