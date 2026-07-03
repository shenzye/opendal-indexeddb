use super::core::IndexeddbCore;
use super::io::{
    IndexeddbDeleter, IndexeddbLimitLister, IndexeddbListRequest, IndexeddbLister, IndexeddbWriter,
    list_page_size,
};
use super::meta::{ReadArgs, WriteArgs, check_etag, check_modified, entry_metadata};
use opendal::raw::{
    Access, AccessorInfo, OpCopier, OpCopy, OpCreateDir, OpList, OpRead, OpRename, OpStat, OpWrite,
    RpCopy, RpCreateDir, RpDelete, RpList, RpRead, RpRename, RpStat, RpWrite, build_abs_path, oio,
};
use opendal::{Buffer, Capability, EntryMode, ErrorKind, Metadata};
use std::sync::Arc;

const SCHEME: &str = "indexeddb";
const DELETE_MAX_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub struct IndexeddbBackend {
    core: Arc<IndexeddbCore>,
    root: String,
    info: Arc<AccessorInfo>,
}

impl IndexeddbBackend {
    pub(super) fn new(core: IndexeddbCore) -> Self {
        let info = AccessorInfo::default();
        info.set_scheme(SCHEME);
        info.set_name(format!("{}-{}", core.db_name, core.object_store_name).as_str());
        info.set_root("/");
        info.set_native_capability(Capability {
            read: true,
            write: true,
            write_can_empty: true,
            write_can_append: true,
            write_with_if_not_exists: true,
            copy: true,
            copy_with_if_not_exists: true,
            copy_with_if_match: true,
            rename: true,
            create_dir: true,
            delete: true,
            delete_with_recursive: true,
            delete_max_size: Some(DELETE_MAX_SIZE),
            stat: true,
            stat_with_if_match: true,
            stat_with_if_none_match: true,
            stat_with_if_modified_since: true,
            stat_with_if_unmodified_since: true,
            list: true,
            list_with_recursive: true,
            list_with_limit: true,
            list_with_start_after: true,
            read_with_if_match: true,
            read_with_if_none_match: true,
            read_with_if_modified_since: true,
            read_with_if_unmodified_since: true,
            write_with_if_match: true,
            write_with_if_none_match: true,
            write_with_content_type: true,
            write_with_user_metadata: true,
            shared: false,
            ..Default::default()
        });

        Self {
            core: Arc::new(core),
            root: "/".to_string(),
            info: Arc::new(info),
        }
    }

    pub(super) fn with_normalized_root(mut self, root: String) -> Self {
        self.info.set_root(&root);
        self.root = root;
        self
    }
}

impl Access for IndexeddbBackend {
    type Reader = Buffer;
    type Writer = IndexeddbWriter;
    type Lister = IndexeddbLimitLister<oio::HierarchyLister<IndexeddbLister>>;
    type Deleter = oio::BatchDeleter<IndexeddbDeleter>;
    type Copier = oio::OneShotCopier;

    fn info(&self) -> Arc<AccessorInfo> {
        self.info.clone()
    }

    async fn create_dir(&self, path: &str, _: OpCreateDir) -> opendal::Result<RpCreateDir> {
        let p = build_abs_path(&self.root, path);

        if p != build_abs_path(&self.root, "") {
            self.core
                .set(
                    &p,
                    Buffer::new(),
                    WriteArgs {
                        append: false,
                        if_not_exists: false,
                        if_match: None,
                        if_none_match: None,
                        meta: None,
                    },
                )
                .await?;
        }

        Ok(RpCreateDir::default())
    }

    async fn stat(&self, path: &str, args: OpStat) -> opendal::Result<RpStat> {
        let p = build_abs_path(&self.root, path);

        if p == build_abs_path(&self.root, "") {
            // The root has no mtime; the conditional checks below can only
            // disprove modification, so without an mtime `if_modified_since`
            // would always fail and `if_unmodified_since` always pass — neither
            // is meaningful for a synthetic root, so we ignore the conditions
            // here as fs/memory backends do.
            return Ok(RpStat::new(Metadata::new(EntryMode::DIR)));
        }
        match self.core.stat(&p).await? {
            Some(info) => {
                check_modified(&info, args.if_modified_since(), args.if_unmodified_since())
                    .map_err(|err| err.with_context("path", path))?;
                check_etag(&info, args.if_match(), args.if_none_match())
                    .map_err(|err| err.with_context("path", path))?;
                Ok(RpStat::new(entry_metadata(path, &info)))
            }
            None => Err(opendal::Error::new(
                ErrorKind::NotFound,
                "indexeddb doesn't have this path",
            )
            .with_context("path", path)),
        }
    }

    async fn read(&self, path: &str, args: OpRead) -> opendal::Result<(RpRead, Self::Reader)> {
        let p = build_abs_path(&self.root, path);
        let (info, bs) = match self.core.read(&p, ReadArgs::from(&args)).await? {
            Some(entry) => entry,
            None => {
                return Err(opendal::Error::new(
                    ErrorKind::NotFound,
                    "indexeddb doesn't have this path",
                )
                .with_context("path", path));
            }
        };

        let meta = entry_metadata(path, &info);

        Ok((RpRead::new(meta), bs))
    }

    async fn write(&self, path: &str, args: OpWrite) -> opendal::Result<(RpWrite, Self::Writer)> {
        let p = build_abs_path(&self.root, path);
        Ok((
            RpWrite::new(),
            IndexeddbWriter::new(self.core.clone(), p, WriteArgs::from(&args)),
        ))
    }

    async fn copy(
        &self,
        from: &str,
        to: &str,
        args: OpCopy,
        _: OpCopier,
    ) -> opendal::Result<(RpCopy, Self::Copier)> {
        let from = build_abs_path(&self.root, from);
        let to = build_abs_path(&self.root, to);
        let core = self.core.clone();
        let if_not_exists = args.if_not_exists();
        let if_match = args.if_match().map(str::to_owned);

        Ok((
            RpCopy::default(),
            oio::OneShotCopier::new(async move {
                core.copy(&from, &to, if_not_exists, if_match.as_deref())
                    .await
            }),
        ))
    }

    async fn rename(&self, from: &str, to: &str, _: OpRename) -> opendal::Result<RpRename> {
        let from = build_abs_path(&self.root, from);
        let to = build_abs_path(&self.root, to);
        self.core.rename(&from, &to).await?;

        Ok(RpRename::default())
    }

    async fn delete(&self) -> opendal::Result<(RpDelete, Self::Deleter)> {
        Ok((
            RpDelete::default(),
            oio::BatchDeleter::new(
                IndexeddbDeleter::new(self.core.clone(), self.root.clone()),
                Some(DELETE_MAX_SIZE),
            ),
        ))
    }

    async fn list(&self, path: &str, args: OpList) -> opendal::Result<(RpList, Self::Lister)> {
        let p = build_abs_path(&self.root, path);
        // `start_after` is a relative path; lift it into the same absolute key
        // space the store uses so the cursor can resume from the next entry.
        let start_after = args.start_after().map(|s| build_abs_path(&self.root, s));
        let skip_folded_dir = if !args.recursive() {
            start_after.as_ref().filter(|p| p.ends_with('/')).cloned()
        } else {
            None
        };
        let lister = IndexeddbLister::new(IndexeddbListRequest {
            core: self.core.clone(),
            root: self.root.clone(),
            prefix: p,
            start_after,
            skip_prefix: skip_folded_dir,
            page_size: list_page_size(args.limit()),
        });
        let lister = oio::HierarchyLister::new(lister, path, args.recursive());
        // `limit` bounds the *emitted* entries, which is what callers observe
        // after HierarchyLister has folded sibling keys into directory entries.
        // Truncating at the raw-key level would miscount under non-recursive
        // listing, so the limit is applied outside the hierarchy layer.
        let lister = IndexeddbLimitLister::new(lister, args.limit());

        Ok((RpList::default(), lister))
    }
}
