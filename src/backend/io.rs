use super::core::IndexeddbCore;
use super::meta::{EntryInfo, ScanPageRequest, WriteArgs, entry_metadata};
use opendal::raw::{OpDelete, build_abs_path, build_rel_path, oio};
use opendal::{Buffer, Metadata};
use std::collections::VecDeque;
use std::sync::Arc;

pub(super) struct IndexeddbListRequest {
    pub(super) core: Arc<IndexeddbCore>,
    pub(super) root: String,
    pub(super) prefix: String,
    pub(super) start_after: Option<String>,
    pub(super) skip_prefix: Option<String>,
    pub(super) page_size: usize,
}

pub(super) fn list_page_size(limit: Option<usize>) -> usize {
    const DEFAULT: usize = 1024;
    match limit {
        Some(limit) => (limit.max(1) * 4).min(DEFAULT),
        None => DEFAULT,
    }
}

pub struct IndexeddbLister {
    core: Arc<IndexeddbCore>,
    root: String,
    prefix: String,
    start_after: Option<String>,
    skip_prefix: Option<String>,
    page_size: usize,
    buffer: VecDeque<(String, EntryInfo)>,
    done: bool,
}

impl IndexeddbLister {
    pub(super) fn new(req: IndexeddbListRequest) -> Self {
        Self {
            core: req.core,
            root: req.root,
            prefix: req.prefix,
            start_after: req.start_after,
            skip_prefix: req.skip_prefix,
            page_size: req.page_size,
            buffer: VecDeque::new(),
            done: false,
        }
    }
}

impl oio::List for IndexeddbLister {
    async fn next(&mut self) -> opendal::Result<Option<oio::Entry>> {
        loop {
            if let Some((key, info)) = self.buffer.pop_front() {
                let mut path = build_rel_path(&self.root, &key);
                if path.is_empty() {
                    path = "/".to_string();
                }
                return Ok(Some(oio::Entry::new(&path, entry_metadata(&path, &info))));
            }

            if self.done {
                return Ok(None);
            }

            let page = self
                .core
                .scan_page(ScanPageRequest {
                    prefix: &self.prefix,
                    start_after: self.start_after.as_deref(),
                    skip_prefix: self.skip_prefix.as_deref(),
                    limit: self.page_size,
                })
                .await?;
            self.start_after = page.next_start_after;
            self.skip_prefix = page.skip_prefix;
            self.done = page.done;
            self.buffer = page.entries.into();

            if self.buffer.is_empty() && self.done {
                return Ok(None);
            }
        }
    }
}

/// Caps the number of entries a lister emits. It sits outside
/// [`oio::HierarchyLister`] so the cap counts the entries callers actually
/// receive — folded directory entries in non-recursive listings — rather than
/// the raw keys underneath, which would undercount when siblings collapse.
pub struct IndexeddbLimitLister<P> {
    inner: P,
    limit: Option<usize>,
    emitted: usize,
}

impl<P> IndexeddbLimitLister<P> {
    pub(super) fn new(inner: P, limit: Option<usize>) -> Self {
        Self {
            inner,
            limit,
            emitted: 0,
        }
    }
}

impl<P: oio::List> oio::List for IndexeddbLimitLister<P> {
    async fn next(&mut self) -> opendal::Result<Option<oio::Entry>> {
        if let Some(limit) = self.limit
            && self.emitted >= limit
        {
            return Ok(None);
        }
        match self.inner.next().await? {
            Some(entry) => {
                self.emitted += 1;
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }
}

pub struct IndexeddbWriter {
    core: Arc<IndexeddbCore>,
    path: String,
    args: WriteArgs,
    buffer: oio::QueueBuf,
}

impl IndexeddbWriter {
    pub(super) fn new(core: Arc<IndexeddbCore>, path: String, args: WriteArgs) -> Self {
        Self {
            core,
            path,
            args,
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
        let info = self.core.set(&self.path, buf, self.args.clone()).await?;
        Ok(entry_metadata(&self.path, &info))
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
    pub(super) fn new(core: Arc<IndexeddbCore>, root: String) -> Self {
        Self { core, root }
    }
}

impl oio::BatchDelete for IndexeddbDeleter {
    async fn delete_once(&self, path: String, args: OpDelete) -> opendal::Result<()> {
        let p = build_abs_path(&self.root, &path);
        if args.recursive() {
            self.core.delete_recursive(&p).await?;
        } else {
            self.core.delete(&p).await?;
        }
        Ok(())
    }

    async fn delete_batch(
        &self,
        batch: Vec<(String, OpDelete)>,
    ) -> opendal::Result<oio::BatchDeleteResult> {
        let paths = batch
            .iter()
            .map(|(path, args)| (build_abs_path(&self.root, path), args.recursive()))
            .collect();
        self.core.delete_many(paths).await?;

        Ok(oio::BatchDeleteResult {
            succeeded: batch,
            failed: Vec::new(),
        })
    }
}
