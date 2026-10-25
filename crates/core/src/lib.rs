//! Hutan domain types: identifiers, session lifecycle, the agent event model, the agent
//! observation tree, tasks with dependencies, checkpoints and the plan of a restore. No I/O
//! lives here.

pub mod agent;
pub mod checkpoint;
pub mod contain;
pub mod ids;
pub mod observation;
pub mod question;
pub mod restore;
pub mod safety;
pub mod session;
pub mod task;

pub use agent::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalPolicy, DecidedBy, Delivery, ModelInfo,
    SandboxMode,
};
pub use checkpoint::{
    CheckpointAvailability, CheckpointInfo, CheckpointKind, CheckpointState, CheckpointStatus,
    SkipReason, SkippedFile,
};
pub use contain::{Contain, ContainKind, Containment};
pub use ids::{
    ApprovalId, CheckpointId, Epoch, ItemId, ParseIdError, QuestionId, Seq, SessionId, TaskId,
    TerminalId, TurnId, WorkspaceId,
};
pub use observation::{
    AgentNode, AgentObservation, Changes, Liveness, NodeKind, NodeOutcome, NodeState, NodeUpdate,
    ObservationTree, Patch, Usage,
};
pub use question::{QuestionOutcome, QuestionResponse, QuestionSpec};
pub use restore::{
    AcceptError, Change, FileMode, PlannedFile, Present, RestoreAction, RestoreEntry, RestorePlan,
    RestoreState, TreeFile, TurnChanges, restore_plan,
};
pub use safety::{SafetyEnforcer, SafetyPreset};
pub use session::{InvalidTransition, SessionEvent, SessionKind, SessionState};
pub use task::{
    AfterPolicy, DELIVERY_HISTORY_MAX, DeliveryActor, DeliveryRecord, DeliveryState,
    InvalidTaskTransition, REVIEW_NOTE_MAX_BYTES, ReviewDecision, Scheduler, Task, TaskEvent,
    TaskPlacement, TaskPlacementKind, TaskRun, TaskState,
};
