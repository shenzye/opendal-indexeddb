use crate::config::IndexeddbConfig;
use opendal::{Buffer, Configurator, Operator};
use std::hint::black_box;
use std::time::Duration;
use wasm_bindgen_test::{Criterion, Instant, console_log, wasm_bindgen_bench};

const SMALL_OBJECT_SIZE: usize = 4 * 1024;
const LARGE_OBJECT_SIZE: usize = 1024 * 1024;
const APPEND_CHUNK_SIZE: usize = 4 * 1024;
const APPEND_TARGET_SIZE: usize = 1024 * 1024;
const FLAT_ENTRY_COUNT: usize = 512;
const NESTED_DIR_COUNT: usize = 16;
const NESTED_FILE_COUNT: usize = 32;
const DELETE_BATCH_SIZE: usize = 100;

#[wasm_bindgen_bench]
async fn indexeddb_backend(c: &mut Criterion) {
    tune(c);

    bench_write_small_object(c).await;
    bench_read_small_object(c).await;
    bench_write_large_object(c).await;
    bench_read_large_object(c).await;
    bench_read_large_object_small_range(c).await;
    bench_stat_existing_object(c).await;
    bench_list_flat_directory(c).await;
    bench_list_flat_directory_limit_one(c).await;
    bench_list_flat_directory_start_after_tail(c).await;
    bench_list_recursive_directory(c).await;
    bench_list_recursive_directory_limit_one(c).await;
    bench_delete_iter_batch(c).await;
    bench_append_4k_0_to_1m().await;
}

fn tune(c: &mut Criterion) {
    *c = std::mem::take(c)
        .sample_size(15)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_millis(1500))
        .nresamples(10_000);
}

async fn bench_write_small_object(c: &mut Criterion) {
    let op = fresh_operator("perf_write_small").await;
    let payload = payload(SMALL_OBJECT_SIZE);
    op.write("object", payload.clone()).await.unwrap();

    c.bench_async_function("indexeddb/write/4KiB", move |b| {
        let op = op.clone();
        let payload = payload.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                let payload = payload.clone();
                async move {
                    op.write("object", payload).await.unwrap();
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_read_small_object(c: &mut Criterion) {
    let op = fresh_operator("perf_read_small").await;
    let payload = payload(SMALL_OBJECT_SIZE);
    op.write("object", payload).await.unwrap();

    c.bench_async_function("indexeddb/read/4KiB", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let content = op.read("object").await.unwrap();
                    black_box(content.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_write_large_object(c: &mut Criterion) {
    let op = fresh_operator("perf_write_large").await;
    let payload = payload(LARGE_OBJECT_SIZE);
    op.write("object", payload.clone()).await.unwrap();

    c.bench_async_function("indexeddb/write/1MiB", move |b| {
        let op = op.clone();
        let payload = payload.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                let payload = payload.clone();
                async move {
                    op.write("object", payload).await.unwrap();
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_read_large_object(c: &mut Criterion) {
    let op = fresh_operator("perf_read_large").await;
    let payload = payload(LARGE_OBJECT_SIZE);
    op.write("object", payload).await.unwrap();

    c.bench_async_function("indexeddb/read/1MiB", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let content = op.read("object").await.unwrap();
                    black_box(content.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_read_large_object_small_range(c: &mut Criterion) {
    let op = fresh_operator("perf_read_large_small_range").await;
    let payload = payload(LARGE_OBJECT_SIZE);
    op.write("object", payload).await.unwrap();

    c.bench_async_function("indexeddb/read/1MiB-range-4KiB", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let content = op
                        .read_with("object")
                        .range(128 * 1024..128 * 1024 + SMALL_OBJECT_SIZE as u64)
                        .await
                        .unwrap();
                    black_box(content.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_stat_existing_object(c: &mut Criterion) {
    let op = fresh_operator("perf_stat").await;
    let payload = payload(SMALL_OBJECT_SIZE);
    op.write("object", payload).await.unwrap();

    c.bench_async_function("indexeddb/stat/existing", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let meta = op.stat("object").await.unwrap();
                    black_box(meta.content_length());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_list_flat_directory(c: &mut Criterion) {
    let op = fresh_operator("perf_list_flat").await;
    populate_flat_directory(&op).await;

    c.bench_async_function("indexeddb/list/flat-512", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let entries = op.list("flat/").await.unwrap();
                    black_box(entries.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_list_flat_directory_limit_one(c: &mut Criterion) {
    let op = fresh_operator("perf_list_flat_limit_one").await;
    populate_flat_directory(&op).await;

    c.bench_async_function("indexeddb/list/flat-512-limit-1", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let entries = op.list_with("flat/").limit(1).await.unwrap();
                    black_box(entries.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_list_flat_directory_start_after_tail(c: &mut Criterion) {
    let op = fresh_operator("perf_list_flat_start_after_tail").await;
    populate_flat_directory(&op).await;

    c.bench_async_function("indexeddb/list/flat-512-start-after-tail", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let entries = op
                        .list_with("flat/")
                        .start_after("flat/file-500")
                        .await
                        .unwrap();
                    black_box(entries.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_list_recursive_directory(c: &mut Criterion) {
    let op = fresh_operator("perf_list_recursive").await;
    populate_nested_directory(&op).await;

    c.bench_async_function("indexeddb/list/recursive-512", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let entries = op.list_with("nested/").recursive(true).await.unwrap();
                    black_box(entries.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_list_recursive_directory_limit_one(c: &mut Criterion) {
    let op = fresh_operator("perf_list_recursive_limit_one").await;
    populate_nested_directory(&op).await;

    c.bench_async_function("indexeddb/list/recursive-512-limit-1", move |b| {
        let op = op.clone();
        Box::pin(async move {
            b.iter_future(|| {
                let op = op.clone();
                async move {
                    let entries = op
                        .list_with("nested/")
                        .recursive(true)
                        .limit(1)
                        .await
                        .unwrap();
                    black_box(entries.len());
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_delete_iter_batch(c: &mut Criterion) {
    let op = fresh_operator("perf_delete_iter").await;
    let data = Buffer::from("x");
    let mut batch_id = 0usize;

    c.bench_async_function("indexeddb/delete_iter/100", move |b| {
        let op = op.clone();
        let data = data.clone();
        Box::pin(async move {
            b.iter_custom_future(|iters| {
                let op = op.clone();
                let data = data.clone();
                let start_batch = batch_id;
                batch_id += iters as usize;
                async move {
                    let mut elapsed = Duration::ZERO;
                    for iter in 0..iters as usize {
                        let batch = start_batch + iter;
                        let mut paths = Vec::with_capacity(DELETE_BATCH_SIZE);
                        for idx in 0..DELETE_BATCH_SIZE {
                            let path = format!("delete/batch/{batch:08}/{idx:03}");
                            op.write(path.as_str(), data.clone()).await.unwrap();
                            paths.push(path);
                        }

                        let start = Instant::now();
                        op.delete_iter(paths).await.unwrap();
                        elapsed += start.elapsed();
                    }

                    elapsed
                }
            })
            .await;
        })
    })
    .await;
}

async fn bench_append_4k_0_to_1m() {
    let op = fresh_operator("perf_append_4k_0_to_1m").await;
    let payload = payload(APPEND_CHUNK_SIZE);
    let chunk_count = APPEND_TARGET_SIZE / APPEND_CHUNK_SIZE;
    let start = Instant::now();

    for _ in 0..chunk_count {
        op.write_with("object", payload.clone())
            .append(true)
            .await
            .unwrap();
    }

    let elapsed = start.elapsed();
    let meta = op.stat("object").await.unwrap();
    assert_eq!(meta.content_length(), APPEND_TARGET_SIZE as u64);

    log_append_result(elapsed, chunk_count);
}

async fn fresh_operator(db_name: &str) -> Operator {
    delete_database(db_name).await;
    Operator::new(
        IndexeddbConfig {
            db_name: Some(db_name.to_string()),
            object_store_name: None,
            root: None,
        }
        .into_builder(),
    )
    .unwrap()
    .finish()
}

async fn delete_database(db_name: &str) {
    let factory = indexed_db::Factory::<opendal::Error>::get().unwrap();
    factory.delete_database(db_name).await.unwrap();
}

async fn populate_flat_directory(op: &Operator) {
    let data = Buffer::from("x");
    for idx in 0..FLAT_ENTRY_COUNT {
        let path = format!("flat/file-{idx:03}");
        op.write(path.as_str(), data.clone()).await.unwrap();
    }
    assert_eq!(op.list("flat/").await.unwrap().len(), FLAT_ENTRY_COUNT);
}

async fn populate_nested_directory(op: &Operator) {
    let data = Buffer::from("x");
    for dir in 0..NESTED_DIR_COUNT {
        for file in 0..NESTED_FILE_COUNT {
            let path = format!("nested/dir-{dir:02}/file-{file:02}");
            op.write(path.as_str(), data.clone()).await.unwrap();
        }
    }
    assert_eq!(
        op.list_with("nested/").recursive(true).await.unwrap().len(),
        NESTED_DIR_COUNT * NESTED_FILE_COUNT
    );
}

fn log_append_result(elapsed: Duration, chunks: usize) {
    let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;
    let written_mib = APPEND_TARGET_SIZE as f64 / 1024.0 / 1024.0;
    let throughput_mib_s = written_mib / elapsed.as_secs_f64();
    let avg_append_ms = elapsed_ms / chunks as f64;

    console_log!(
        "indexeddb/append/4KiB/0_to_1MiB chunks={} elapsed={:.2}ms throughput={:.2}MiB/s avg_append={:.3}ms",
        chunks,
        elapsed_ms,
        throughput_mib_s,
        avg_append_ms
    );
}

fn payload(size: usize) -> Buffer {
    let bytes = (0..size)
        .map(|idx| (idx.wrapping_mul(31) % 251) as u8)
        .collect::<Vec<_>>();
    Buffer::from(bytes)
}
