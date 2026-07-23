use rusqlite::{Connection, Transaction};

use crate::error::CarryCtxError;

/// Wraps a rusqlite immediate-mode transaction.
/// Provides access to the underlying connection for repository operations.
pub struct UnitOfWork<'conn> {
    tx: Option<Transaction<'conn>>,
}

impl<'conn> UnitOfWork<'conn> {
    pub fn new(tx: Transaction<'conn>) -> Self {
        Self { tx: Some(tx) }
    }

    /// Return a reference to the inner Connection.
    pub fn connection(&self) -> &Connection {
        self.tx.as_ref().expect("UnitOfWork already finalized")
    }

    /// Commit the transaction.
    pub fn commit(mut self) -> Result<(), CarryCtxError> {
        let tx = self.tx.take().expect("UnitOfWork already finalized");
        tx.commit().map_err(|e| {
            CarryCtxError::database_error(format!("Transaction commit failed: {e}")).with_source(e)
        })
    }

    /// Roll back the transaction.
    pub fn rollback(mut self) -> Result<(), CarryCtxError> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().map_err(|e| {
                CarryCtxError::database_error(format!("Transaction rollback failed: {e}"))
                    .with_source(e)
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for UnitOfWork<'_> {
    fn drop(&mut self) {
        if self.tx.is_some() {
            let _ = self.tx.take().unwrap().rollback();
        }
    }
}
