//! The board store: a source of envelopes plus the queries over them.
//!
//! A `Store` is a handle — cheap to clone, so background tasks each take
//! their own — over one [`source::Source`]. Reads fold whatever the source
//! returns through [`query`], writes go back to the source. The log-backed
//! source (file reads, `horizon-logd` writes) and the in-memory one
//! therefore answer the same questions with the same code.

mod memory;
mod query;
mod source;
mod types;

#[cfg(not(target_family = "wasm"))]
mod file;
#[cfg(not(target_family = "wasm"))]
mod logd;
#[cfg(not(target_family = "wasm"))]
mod open;

#[cfg(test)]
mod tests;

use crate::event::ReadReport;
use crate::model::Item;
use crate::wire::{IngestReply, IngestRequest};
use source::Source;

pub use memory::sample_envelopes;
pub use types::{ListResult, Position, StoreError};

#[cfg(not(target_family = "wasm"))]
pub use file::read as read_events;
#[cfg(not(target_family = "wasm"))]
pub use logd::SubscribeStream;

/// The board store. Every read folds the source's current envelopes; every
/// write is one operation the source performs (or refuses).
#[derive(Clone)]
pub struct Store {
    source: Source,
}

impl Store {
    /// The events.jsonl path this store reads from and tells logd to append
    /// to. Empty for an in-memory store, which has no file.
    pub fn path(&self) -> &std::path::Path {
        self.source.path()
    }

    // -- reads ----------------------------------------------------------

    /// The raw read report: the envelopes and the tolerant reader's counts.
    pub fn events(&self) -> Result<ReadReport, StoreError> {
        self.source.report()
    }

    /// Lists items by rank, optionally filtered by project-defined state.
    /// Without an explicit state filter, closed tasks are hidden unless
    /// `include_closed` is true. Returns the full observed state vocabulary.
    pub fn list(
        &self,
        status_filter: Option<&str>,
        include_closed: bool,
    ) -> Result<ListResult, StoreError> {
        Ok(query::list(&self.events()?, status_filter, include_closed))
    }

    /// Returns the full item (with comments) or `None` if the id doesn't exist.
    pub fn show(&self, id: u64) -> Result<Option<Item>, StoreError> {
        Ok(query::show(&self.events()?.envelopes, id))
    }

    /// The furthest position `consumer` has been advanced to.
    pub fn cursor(&self, consumer: &str) -> Result<u64, StoreError> {
        Ok(query::cursor(&self.events()?.envelopes, consumer))
    }

    /// Furthest read message per task, ordered by the task's comment sequence.
    /// Existing individual read events collapse into one inclusive read prefix.
    pub fn read_positions(
        &self,
        reader: &str,
    ) -> Result<std::collections::HashMap<u64, String>, StoreError> {
        Ok(query::read_positions(&self.events()?.envelopes, reader))
    }

    // -- writes ---------------------------------------------------------
    //
    // The write methods are `async fn`: on a log-backed store each makes one
    // remoc rtc round-trip to `horizon-logd`. A source that accepts no
    // writes (`Store::in_memory`) fails them with `StoreError::ReadOnly`
    // without contacting anything.

    /// Creates a task at a validated position among its siblings.
    pub async fn add(
        &self,
        title: &str,
        body: &str,
        parent: Option<u64>,
        position: Position,
    ) -> Result<Item, StoreError> {
        let reply = self
            .ingest(IngestRequest::Add {
                title: title.to_string(),
                body: body.to_string(),
                parent,
                position,
            })
            .await?;
        match reply {
            IngestReply::Item(item) => Ok(item),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Appends a comment to item `id`.
    pub async fn comment(&self, id: u64, author: &str, text: &str) -> Result<(), StoreError> {
        let reply = self
            .ingest(IngestRequest::Comment {
                id,
                author: author.to_string(),
                text: text.to_string(),
            })
            .await?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Sets project-defined progress text independently of closure.
    pub async fn set_status(&self, id: u64, status: &str) -> Result<(), StoreError> {
        let reply = self
            .ingest(IngestRequest::SetStatus {
                id,
                status: status.to_string(),
            })
            .await?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Re-ranks item `id` to a new position in the queue.
    pub async fn move_item(&self, id: u64, position: Position) -> Result<String, StoreError> {
        let reply = self
            .ingest(IngestRequest::MoveItem { id, position })
            .await?;
        match reply {
            IngestReply::Rank(rank) => Ok(rank),
            _ => Err(Self::type_mismatch()),
        }
    }

    /// Updates an item's title and/or body. Pass `None` for either field to
    /// leave it unchanged. The daemon folds and edits under one lock so
    /// a partial edit cannot clobber another concurrently changed field.
    pub async fn edit(
        &self,
        id: u64,
        title: Option<String>,
        body: Option<String>,
    ) -> Result<(), StoreError> {
        let reply = self.ingest(IngestRequest::Edit { id, title, body }).await?;
        match reply {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    pub async fn set_parent(
        &self,
        id: u64,
        parent: Option<u64>,
        position: Position,
    ) -> Result<(), StoreError> {
        self.done(IngestRequest::SetParent {
            id,
            parent,
            position,
        })
        .await
    }

    /// Closes or reopens a task, optionally updating its progress text in the
    /// same transaction. Neither operation infers the flag from status text.
    pub async fn set_closed(
        &self,
        id: u64,
        is_closed: bool,
        status: Option<&str>,
    ) -> Result<(), StoreError> {
        self.done(IngestRequest::SetClosed {
            id,
            is_closed,
            status: status.map(str::to_owned),
        })
        .await
    }

    pub async fn set_dependencies(&self, id: u64, depends_on: Vec<u64>) -> Result<(), StoreError> {
        self.done(IngestRequest::SetDependencies { id, depends_on })
            .await
    }

    pub async fn post_message(&self, id: u64, message: crate::Comment) -> Result<(), StoreError> {
        self.done(IngestRequest::PostMessage { id, message }).await
    }

    pub async fn mark_read(
        &self,
        id: u64,
        reader: &str,
        message_id: &str,
    ) -> Result<(), StoreError> {
        self.done(IngestRequest::MarkRead {
            id,
            reader: reader.into(),
            message_id: message_id.into(),
        })
        .await
    }

    pub async fn advance_cursor(&self, consumer: &str, position: u64) -> Result<(), StoreError> {
        self.done(IngestRequest::AdvanceCursor {
            consumer: consumer.into(),
            position,
        })
        .await
    }

    pub async fn bind_session(&self, id: u64, session_id: &str) -> Result<Item, StoreError> {
        self.bind(id, session_id, false).await
    }

    pub async fn bind_review_session(&self, id: u64, session_id: &str) -> Result<Item, StoreError> {
        self.bind(id, session_id, true).await
    }

    async fn bind(&self, id: u64, session_id: &str, review: bool) -> Result<Item, StoreError> {
        match self
            .ingest(IngestRequest::BindSession {
                id,
                session_id: session_id.into(),
                review,
            })
            .await?
        {
            IngestReply::Item(item) => Ok(item),
            _ => Err(Self::type_mismatch()),
        }
    }

    // -- internals ------------------------------------------------------

    async fn done(&self, request: IngestRequest) -> Result<(), StoreError> {
        match self.ingest(request).await? {
            IngestReply::Done => Ok(()),
            _ => Err(Self::type_mismatch()),
        }
    }

    async fn ingest(&self, request: IngestRequest) -> Result<IngestReply, StoreError> {
        self.source.ingest(request).await
    }

    #[cold]
    fn type_mismatch() -> StoreError {
        StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "logd returned an unexpected reply type for this request",
        ))
    }
}
