use chrono::{DateTime, Utc};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeCheck {
    pub(crate) runtime: String,
    pub(crate) command: String,
    pub(crate) available: bool,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Agent {
    pub(crate) id: Uuid,
    pub(crate) handle: String,
    pub(crate) display_name: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) runtime: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
    pub(crate) service_tier: String,
    pub(crate) avatar: String,
    pub(crate) description: String,
    pub(crate) launch_command: String,
    pub(crate) environment_variables: String,
    pub(crate) working_directory: String,
    pub(crate) workspace_exists: bool,
    pub(crate) workspace_memory_path: String,
    pub(crate) workspace_memory_exists: bool,
    pub(crate) workspace_entries: Vec<AgentWorkspaceEntry>,
    pub(crate) details_loaded: bool,
    pub(crate) daily_budget_micros: i64,
    pub(crate) subscription_status: Option<AgentSubscriptionStatus>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentSubscriptionStatus {
    pub(crate) provider: String,
    pub(crate) plan: Option<String>,
    pub(crate) status: String,
    pub(crate) windows: Vec<AgentSubscriptionWindow>,
    pub(crate) observed_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub(crate) struct AgentSubscriptionWindow {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) used_percent: f64,
    pub(crate) resets_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OwnerProfile {
    pub(crate) display_name: String,
    pub(crate) avatar: String,
    pub(crate) description: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentWorkspaceEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) relative_path: String,
    pub(crate) kind: String,
    pub(crate) size_bytes: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentWorkspaceListing {
    pub(crate) path: String,
    pub(crate) entries: Vec<AgentWorkspaceEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentWorkspaceFile {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) relative_path: String,
    pub(crate) size_bytes: i64,
    pub(crate) language: String,
    pub(crate) content: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Channel {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) kind: String,
    pub(crate) dm_agent_id: Option<Uuid>,
    pub(crate) unread_count: i32,
    pub(crate) github_unread_count: i32,
    pub(crate) github_review_synced_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ThreadActivity {
    pub(crate) reply_count: i64,
    pub(crate) thread_root_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) unread_count: i32,
    pub(crate) latest_message_id: Uuid,
    pub(crate) latest_activity_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChannelMember {
    pub(crate) channel_id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) agent_display_name: String,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Message {
    pub(crate) id: Uuid,
    pub(crate) seq: i64,
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) sender_agent_id: Option<Uuid>,
    pub(crate) sender_name: String,
    pub(crate) sender_role: String,
    pub(crate) body: String,
    pub(crate) is_task: bool,
    pub(crate) thread_followed: bool,
    pub(crate) delivery_state: String,
    pub(crate) stream_key: String,
    pub(crate) task_number: Option<i64>,
    pub(crate) task_status: Option<String>,
    pub(crate) attachments: Vec<MessageAttachment>,
    pub(crate) artifacts: Vec<Artifact>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChannelMessageHistory {
    pub(crate) channel_id: Uuid,
    pub(crate) before_seq: Option<i64>,
    pub(crate) has_more: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChannelMessagePage {
    pub(crate) messages: Vec<Message>,
    pub(crate) next_before_seq: Option<i64>,
    pub(crate) has_more: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedMessage {
    pub(crate) id: Uuid,
    pub(crate) message_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) channel_name: String,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) sender_name: String,
    pub(crate) sender_role: String,
    pub(crate) body: String,
    pub(crate) message_created_at: DateTime<Utc>,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MessageAttachment {
    pub(crate) id: Uuid,
    pub(crate) message_id: Uuid,
    pub(crate) original_name: String,
    pub(crate) mime_type: String,
    pub(crate) size_bytes: i64,
    pub(crate) storage_path: String,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Artifact {
    pub(crate) id: Uuid,
    pub(crate) message_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) creator_agent_id: Option<Uuid>,
    pub(crate) creator_agent_handle: Option<String>,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) content: String,
    pub(crate) metadata: Value,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentUpload {
    pub(crate) original_name: String,
    pub(crate) mime_type: String,
    pub(crate) bytes: Vec<u8>,
    // Only the multipart parser can supply a local file. Never deserialize paths
    // from JSON/Tauri requests.
    #[serde(skip)]
    pub(crate) staged: Option<crate::attachments::StagedAttachment>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Task {
    pub(crate) id: Uuid,
    pub(crate) number: i64,
    pub(crate) message_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) version: i64,
    pub(crate) channel_name: String,
    pub(crate) assignee_id: Option<Uuid>,
    pub(crate) assignee_name: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Reminder {
    pub(crate) id: Uuid,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) channel_name: Option<String>,
    pub(crate) creator_agent_id: Option<Uuid>,
    pub(crate) creator_agent_handle: Option<String>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) message_id: Option<Uuid>,
    pub(crate) title: String,
    pub(crate) note: String,
    pub(crate) status: String,
    pub(crate) recurrence: String,
    pub(crate) due_at: DateTime<Utc>,
    pub(crate) fired_at: Option<DateTime<Utc>>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentSchedule {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) channel_id: Uuid,
    pub(crate) channel_name: String,
    pub(crate) channel_kind: String,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) cadence: String,
    pub(crate) status: String,
    pub(crate) next_run_at: DateTime<Utc>,
    pub(crate) last_run_at: Option<DateTime<Utc>>,
    pub(crate) last_work_item_id: Option<Uuid>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentRun {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) work_item_id: Option<Uuid>,
    pub(crate) command: String,
    pub(crate) working_directory: String,
    pub(crate) status: String,
    pub(crate) pid: Option<i32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) log: String,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cost_micros: i64,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentRunPatch {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) work_item_id: Option<Uuid>,
    pub(crate) command: String,
    pub(crate) working_directory: String,
    pub(crate) status: String,
    pub(crate) pid: Option<i32>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cost_micros: i64,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentActivity {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Option<Uuid>,
    pub(crate) agent_handle: String,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) kind: String,
    pub(crate) phase: String,
    pub(crate) status: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) metadata: Value,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentWorkItem {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) channel_name: Option<String>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) source_message_id: Option<Uuid>,
    pub(crate) inbox_item_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) task_number: Option<i64>,
    pub(crate) source_kind: String,
    pub(crate) title: String,
    pub(crate) context: String,
    pub(crate) status: String,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentWorkItemPatch {
    pub(crate) id: Uuid,
    pub(crate) agent_id: Uuid,
    pub(crate) agent_handle: String,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) channel_name: Option<String>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) source_message_id: Option<Uuid>,
    pub(crate) inbox_item_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) task_number: Option<i64>,
    pub(crate) source_kind: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SupervisorStatus {
    pub(crate) pid: Option<i32>,
    pub(crate) status: String,
    pub(crate) updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LaunchAgentStatus {
    pub(crate) label: String,
    pub(crate) plist_path: String,
    pub(crate) installed: bool,
    pub(crate) loaded: bool,
}

#[derive(Debug)]
pub(crate) struct SupervisorCommand {
    pub(crate) id: Uuid,
    pub(crate) command_type: String,
    pub(crate) agent_id: Option<Uuid>,
    pub(crate) run_id: Option<Uuid>,
    pub(crate) work_item_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BootstrapPerfPhase {
    pub(crate) name: String,
    pub(crate) duration_ms: f64,
    pub(crate) rows: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BootstrapPerfOptions {
    pub(crate) runtime: String,
    pub(crate) message_load: String,
    pub(crate) include_run_logs: bool,
    pub(crate) compact_agent_activities: bool,
    pub(crate) include_artifact_content: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct BootstrapPerfCounts {
    pub(crate) channels: usize,
    pub(crate) thread_activities: usize,
    pub(crate) channel_members: usize,
    pub(crate) agents: usize,
    pub(crate) messages: usize,
    pub(crate) saved_messages: usize,
    pub(crate) dismissed_inbox_items: usize,
    pub(crate) read_inbox_items: usize,
    pub(crate) artifacts: usize,
    pub(crate) tasks: usize,
    pub(crate) reminders: usize,
    pub(crate) agent_schedules: usize,
    pub(crate) agent_runs: usize,
    pub(crate) agent_work_items: usize,
    pub(crate) agent_activities: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct BootstrapPerf {
    pub(crate) options: BootstrapPerfOptions,
    pub(crate) total_ms: f64,
    pub(crate) serialize_ms: Option<f64>,
    pub(crate) payload_bytes: Option<usize>,
    pub(crate) phases: Vec<BootstrapPerfPhase>,
    pub(crate) counts: BootstrapPerfCounts,
}

#[derive(Debug, Serialize)]
pub(crate) struct Bootstrap {
    pub(crate) db_url: String,
    pub(crate) web_base_url: Option<String>,
    pub(crate) owner_profile: OwnerProfile,
    pub(crate) channels: Vec<Channel>,
    pub(crate) thread_activities: Vec<ThreadActivity>,
    pub(crate) channel_members: Vec<ChannelMember>,
    pub(crate) agents: Vec<Agent>,
    pub(crate) messages: Vec<Message>,
    pub(crate) channel_message_history: Vec<ChannelMessageHistory>,
    pub(crate) saved_messages: Vec<SavedMessage>,
    pub(crate) dismissed_inbox_items: HashMap<String, DateTime<Utc>>,
    pub(crate) read_inbox_items: HashMap<String, DateTime<Utc>>,
    pub(crate) artifacts: Vec<Artifact>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) reminders: Vec<Reminder>,
    pub(crate) agent_schedules: Vec<AgentSchedule>,
    pub(crate) agent_runs: Vec<AgentRun>,
    pub(crate) agent_work_items: Vec<AgentWorkItem>,
    pub(crate) agent_activities: Vec<AgentActivity>,
    pub(crate) supervisor: SupervisorStatus,
    pub(crate) launch_agent: LaunchAgentStatus,
    pub(crate) ui_event_cursor: i64,
    #[serde(rename = "__perf")]
    pub(crate) perf: Option<BootstrapPerf>,
}
