use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::unit_of_work::UnitOfWork;
use crate::error::CarryCtxError;
use crate::repository::event::{EventFilter, EventRecord, EventRepository};

/// Result of listing events with cursor-based pagination
#[derive(serde::Serialize)]
pub struct CursorList {
    pub events: Vec<EventRecord>,
    pub next_cursor: Option<String>,
}

/// List events with cursor-based pagination.
///
/// The cursor is the `occurred_at` timestamp of the last event in the previous
/// page. Pass it as `until` in the filter to get the next page.
pub fn list_events(
    project_id: &str,
    filter: &EventFilter,
    cursor: Option<&str>,
    uow: &UnitOfWork,
) -> Result<CursorList, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteEventRepository::new(conn);

    // Build an adjusted filter: if cursor is set, use it as the upper bound
    let adjusted_limit = filter.limit.map(|l| l + 1);
    let adjusted_filter = EventFilter {
        project_id: project_id.to_string(),
        task_id: filter.task_id.clone(),
        agent_id: filter.agent_id.clone(),
        session_id: filter.session_id.clone(),
        event_type: filter.event_type.clone(),
        since: filter.since.clone(),
        until: cursor
            .map(|c| c.to_string())
            .or_else(|| filter.until.clone()),
        limit: adjusted_limit,
    };

    let mut events = repo.list(&adjusted_filter)?;

    // Determine next cursor
    let next_cursor = if let Some(limit) = filter.limit {
        let limit = limit as usize;
        if events.len() > limit {
            // Truncate to limit; the (limit+1)th event tells us there's a next page
            events.truncate(limit);
            events.last().map(|e| e.occurred_at.clone())
        } else {
            None
        }
    } else {
        None
    };

    Ok(CursorList {
        events,
        next_cursor,
    })
}

/// Show a single event by ID
pub fn show_event(
    project_id: &str,
    event_id: &str,
    uow: &UnitOfWork,
) -> Result<EventRecord, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteEventRepository::new(conn);

    let event = repo.find_by_id(project_id, event_id)?;
    event.ok_or_else(|| CarryCtxError::resource_not_found(format!("Event '{event_id}' not found.")))
}
