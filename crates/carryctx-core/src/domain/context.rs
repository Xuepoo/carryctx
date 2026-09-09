/// Context relevance group ordering (1-12)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextGroup {
    Task = 1,
    Checkpoint = 2,
    Git = 3,
    Blockers = 4,
    Todos = 5,
    Dependencies = 6,
    BlockedTasks = 7,
    ScopeOverlaps = 8,
    Decisions = 9,
    AgentTasks = 10,
    TaskEvents = 11,
    ProjectActivity = 12,
}

impl ContextGroup {
    pub fn order(self) -> u32 {
        self as u32
    }
}
