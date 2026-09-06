import type {
  AgentWorkspaceFile,
  AgentWorkspaceListing,
  Artifact,
  Bootstrap,
  ChannelMessagePage,
  ChannelWikiOverview,
  ChannelWikiSearchHit,
  GithubChannelOverview,
  GithubReviewComparisons,
  GithubIssueDetail,
  GithubIssueTaskResult,
  GithubRepositoryBinding,
  GithubRereviewTaskResult,
  GithubReviewTaskResult,
  LaunchAgentStatus,
  Message,
  PublishChannelWikiResult,
  RuntimeCheck,
} from "./types";

export type AttachmentUpload = {
  originalName: string;
  mimeType: string;
  bytes: number[];
};

export type InboxItemTimestamp = {
  itemId: string;
  dismissedUntil: string;
};

type MutationResult = void | { ok: true };

type AgentDraftRequest = {
  handle: string;
  displayName: string;
  role?: string | null;
  runtime: string;
  model: string;
  reasoningEffort?: string | null;
  serviceTier?: string | null;
  avatar?: string | null;
  launchCommand: string;
  environmentVariables?: string | null;
  workingDirectory: string;
  dailyBudgetMicros?: number | null;
};

export type ApiContract = {
  bootstrap: {
    args: {
      channelId?: string;
      currentChannelOnly?: boolean;
    };
    result: Bootstrap;
  };
  check_runtime: {
    args: { runtime: string };
    result: RuntimeCheck;
  };
  send_message: {
    args: {
      channelId: string;
      threadRootId?: string | null;
      body: string;
      asTask: boolean;
      attachments?: AttachmentUpload[] | null;
    };
    result: Message;
  };
  load_older_channel_messages: {
    args: {
      channelId: string;
      beforeSeq: number;
      limit: number;
    };
    result: ChannelMessagePage;
  };
  load_channel_messages: {
    args: { channelId: string };
    result: ChannelMessagePage;
  };
  load_channel_previews: {
    args: undefined;
    result: Message[];
  };
  load_activity_messages: {
    args: { mentionHandles: string[] };
    result: Message[];
  };
  search_messages: {
    args: {
      query: string;
      after?: string | null;
      limit: number;
    };
    result: Message[];
  };
  load_message: {
    args: { messageId: string };
    result: Message;
  };
  create_channel: {
    args: {
      name: string;
      description?: string | null;
      agentIds?: string[] | null;
    };
    result: { channelId: string; ok?: true };
  };
  update_channel: {
    args: {
      channelId: string;
      name: string;
      description: string;
    };
    result: MutationResult;
  };
  delete_channel: {
    args: { channelId: string };
    result: MutationResult;
  };
  load_channel_wiki: {
    args: { channelId: string };
    result: ChannelWikiOverview;
  };
  search_channel_wikis: {
    args: {
      query: string;
      limit?: number | null;
    };
    result: ChannelWikiSearchHit[];
  };
  publish_channel_wiki: {
    args: {
      channelId: string;
      parentId?: string | null;
      content: string;
      note: string;
    };
    result: PublishChannelWikiResult;
  };
  load_github_review_comparisons: {
    args: { channelId: string };
    result: GithubReviewComparisons;
  };
  load_github_review_queue: {
    args: { channelId: string };
    result: GithubChannelOverview;
  };
  refresh_github_review_queue: {
    args: { channelId: string };
    result: GithubChannelOverview;
  };
  refresh_github_issue_queue: {
    args: { channelId: string };
    result: GithubChannelOverview;
  };
  mark_github_review_attention_read: {
    args: { channelId: string };
    result: MutationResult;
  };
  load_github_issue_detail: {
    args: {
      channelId: string;
      issueNumber: number;
    };
    result: GithubIssueDetail;
  };
  bind_github_repository: {
    args: {
      channelId: string;
      repository: string;
      localPath?: string | null;
      reviewLogin?: string | null;
    };
    result: GithubRepositoryBinding;
  };
  create_github_review_task: {
    args: {
      channelId: string;
      pullNumber: number;
      agentId: string;
    };
    result: GithubReviewTaskResult;
  };
  rereview_github_pull_request: {
    args: {
      channelId: string;
      pullNumber: number;
    };
    result: GithubRereviewTaskResult;
  };
  create_github_issue_task: {
    args: {
      channelId: string;
      issueNumber: number;
      agentId: string;
    };
    result: GithubIssueTaskResult;
  };
  create_agent: {
    args: AgentDraftRequest & {
      description?: string | null;
    };
    result: string;
  };
  update_agent: {
    args: AgentDraftRequest & {
      agentId: string;
      description: string;
    };
    result: MutationResult;
  };
  delete_agent: {
    args: { agentId: string };
    result: MutationResult;
  };
  start_agent: {
    args: { agentId: string };
    result: MutationResult;
  };
  set_channel_agent_membership: {
    args: {
      channelId: string;
      agentId: string;
      member: boolean;
    };
    result: MutationResult;
  };
  set_message_saved: {
    args: {
      messageId: string;
      saved: boolean;
    };
    result: MutationResult;
  };
  update_owner_profile: {
    args: {
      displayName: string;
      avatar: string;
      description: string;
    };
    result: MutationResult;
  };
  dismiss_inbox_items: {
    args: { items: InboxItemTimestamp[] };
    result: MutationResult;
  };
  mark_inbox_items_read: {
    args: { items: InboxItemTimestamp[] };
    result: MutationResult;
  };
  mark_all_inbox_read: {
    args: undefined;
    result: MutationResult;
  };
  mark_channel_read: {
    args: { channelId: string };
    result: MutationResult;
  };
  complete_reminder: {
    args: { reminderId: string };
    result: MutationResult;
  };
  update_task_status: {
    args: {
      taskId: string;
      status: string;
    };
    result: MutationResult;
  };
  update_task_title: {
    args: {
      taskId: string;
      title: string;
    };
    result: MutationResult;
  };
  claim_task: {
    args: {
      taskId: string;
      agentId?: string | null;
      expectedVersion?: number | null;
    };
    result: MutationResult;
  };
  cancel_agent_work: {
    args: { workItemId: string };
    result: MutationResult;
  };
  retry_agent_work: {
    args: { workItemId: string };
    result: string | { workItemId: string };
  };
  install_supervisor_service: {
    args: undefined;
    result: LaunchAgentStatus;
  };
  uninstall_supervisor_service: {
    args: undefined;
    result: LaunchAgentStatus;
  };
  artifact_read: {
    args: { artifactId: string };
    result: Artifact;
  };
  open_dm_with_agent: {
    args: { agentId: string };
    result: string;
  };
  agent_workspace_list: {
    args: {
      agentId: string;
      path: string;
    };
    result: AgentWorkspaceListing;
  };
  agent_workspace_read_file: {
    args: {
      agentId: string;
      path: string;
    };
    result: AgentWorkspaceFile;
  };
};

const API_COMMAND_NAMES = {
  bootstrap: true,
  check_runtime: true,
  send_message: true,
  load_older_channel_messages: true,
  load_channel_messages: true,
  load_channel_previews: true,
  load_activity_messages: true,
  search_messages: true,
  load_message: true,
  create_channel: true,
  update_channel: true,
  delete_channel: true,
  load_channel_wiki: true,
  publish_channel_wiki: true,
  search_channel_wikis: true,
  load_github_review_comparisons: true,
  load_github_review_queue: true,
  refresh_github_review_queue: true,
  refresh_github_issue_queue: true,
  mark_github_review_attention_read: true,
  load_github_issue_detail: true,
  bind_github_repository: true,
  create_github_review_task: true,
  rereview_github_pull_request: true,
  create_github_issue_task: true,
  create_agent: true,
  update_agent: true,
  delete_agent: true,
  start_agent: true,
  set_channel_agent_membership: true,
  set_message_saved: true,
  update_owner_profile: true,
  dismiss_inbox_items: true,
  mark_inbox_items_read: true,
  mark_all_inbox_read: true,
  mark_channel_read: true,
  complete_reminder: true,
  update_task_status: true,
  update_task_title: true,
  claim_task: true,
  cancel_agent_work: true,
  retry_agent_work: true,
  install_supervisor_service: true,
  uninstall_supervisor_service: true,
  artifact_read: true,
  open_dm_with_agent: true,
  agent_workspace_list: true,
  agent_workspace_read_file: true,
} as const satisfies Record<keyof ApiContract, true>;

export type ApiCommand = keyof ApiContract;
export type ApiArgs<C extends ApiCommand> = ApiContract[C]["args"];
export type ApiResult<C extends ApiCommand> = ApiContract[C]["result"];
export type ApiArgsTuple<C extends ApiCommand> = ApiArgs<C> extends undefined
  ? [args?: undefined]
  : [args: ApiArgs<C>];

export const API_COMMANDS = Object.freeze(
  Object.keys(API_COMMAND_NAMES) as ApiCommand[],
);
