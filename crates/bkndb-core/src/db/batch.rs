//! Cross-model batch view: owns one write tx spanning graph + relational +
//! raw KV together, committed atomically by [`crate::db::Db::write_tx`] only
//! if its closure returns `Ok`. Mirrors [`crate::relational::txn::RelWriteBatch`]'s
//! and [`crate::graph::txn::GraphWriteBatch`]'s "owns `W`, no lifetime
//! parameter on the struct" shape exactly — that shape is what makes the
//! `for<'w> FnOnce(&mut DbWriteBatch<B::WriteTx<'w>>)` closure signature in
//! [`crate::db::write_tx`] compile (an earlier attempt at owning `&'w mut W`
//! instead hit an unsolvable self-referential-lifetime error, since the
//! HRTB then forces `W`'s own lifetime parameter to equal the borrow
//! lifetime of the batch itself).
//!
//! Only one of `.graph()`/`.relational()`/`.kv()` can be borrowed at a time
//! (each takes `&mut self`) — call, use the returned owned values (`NodeId`,
//! `PropValue`, ...; none of them borrow from the batch), then call the
//! next accessor, the same call-then-drop-then-call-next pattern
//! `RelWriteBatch::table()` already requires today.

use crate::kv::txn::BatchKv;
use crate::StorageWriteTx;

pub struct DbWriteBatch<W: StorageWriteTx> {
    wtx: W,
}

impl<W: StorageWriteTx> DbWriteBatch<W> {
    pub(crate) fn new(wtx: W) -> Self {
        Self { wtx }
    }

    pub(crate) fn into_inner(self) -> W {
        self.wtx
    }

    /// Raw KV access, validated against [`crate::RESERVED_TABLE_NAMES`],
    /// over this batch's shared transaction.
    pub fn kv(&mut self) -> BatchKv<'_, W> {
        BatchKv::new(&mut self.wtx)
    }

    #[cfg(feature = "graph")]
    pub fn graph(&mut self) -> crate::graph::txn::BatchGraph<'_, W> {
        crate::graph::txn::BatchGraph::new(&mut self.wtx)
    }

    #[cfg(feature = "relational")]
    pub fn relational(&mut self) -> crate::relational::RelBatchView<'_, W> {
        crate::relational::RelBatchView::new(&mut self.wtx)
    }
}
