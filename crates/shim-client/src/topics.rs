//! Task event topic typically used in shim implementations.

pub const TASK_CREATE_EVENT_TOPIC: &str = "/tasks/create";
pub const TASK_START_EVENT_TOPIC: &str = "/tasks/start";
pub const TASK_OOM_EVENT_TOPIC: &str = "/tasks/oom";
pub const TASK_EXIT_EVENT_TOPIC: &str = "/tasks/exit";
pub const TASK_DELETE_EVENT_TOPIC: &str = "/tasks/delete";
pub const TASK_EXEC_ADDED_EVENT_TOPIC: &str = "/tasks/exec-added";
pub const TASK_EXEC_STARTED_EVENT_TOPIC: &str = "/tasks/exec-started";
pub const TASK_PAUSED_EVENT_TOPIC: &str = "/tasks/paused";
pub const TASK_RESUMED_EVENT_TOPIC: &str = "/tasks/resumed";
pub const TASK_CHECKPOINTED_EVENT_TOPIC: &str = "/tasks/checkpointed";
pub const TASK_UNKNOWN_TOPIC: &str = "/tasks/?";
