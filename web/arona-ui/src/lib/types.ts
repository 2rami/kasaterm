// Types for Claude Code session jsonl events.
// Source of truth: ~/.claude/projects/<encoded-path>/<session-uuid>.jsonl

export type Role = "user" | "assistant" | "system";

export interface ContentBlockText {
  type: "text";
  text: string;
}

export interface ContentBlockThinking {
  type: "thinking";
  thinking: string;
}

export interface ContentBlockToolUse {
  type: "tool_use";
  id: string;
  name: string;
  input: unknown;
}

export interface ContentBlockToolResult {
  type: "tool_result";
  tool_use_id: string;
  content?:
    | string
    | Array<{ type: string; text?: string; [k: string]: unknown }>;
  is_error?: boolean;
}

export type ContentBlock =
  | ContentBlockText
  | ContentBlockThinking
  | ContentBlockToolUse
  | ContentBlockToolResult
  | { type: string;[k: string]: unknown };

export interface AnthropicMessage {
  role: Role;
  content: string | ContentBlock[];
  model?: string;
  id?: string;
  type?: string;
  stop_reason?: string | null;
  usage?: Record<string, unknown>;
}

interface BaseEvent {
  parentUuid?: string | null;
  isSidechain?: boolean;
  type: string;
  uuid?: string;
  timestamp?: string;
  userType?: string;
  entrypoint?: string;
  cwd?: string;
  sessionId?: string;
  version?: string;
  gitBranch?: string;
}

export interface UserEvent extends BaseEvent {
  type: "user";
  message: AnthropicMessage;
  promptId?: string;
  permissionMode?: string;
}

export interface AssistantEvent extends BaseEvent {
  type: "assistant";
  message: AnthropicMessage;
  requestId?: string;
}

export interface SystemEvent extends BaseEvent {
  type: "system";
  subtype?: string;
  level?: string;
  hookCount?: number;
  hookInfos?: Array<{ command: string; durationMs: number }>;
  hookErrors?: unknown[];
  preventedContinuation?: boolean;
  stopReason?: string;
  hasOutput?: boolean;
  toolUseID?: string;
  slug?: string;
  content?: unknown;
}

export interface AttachmentEvent extends BaseEvent {
  type: "attachment";
  attachment: {
    type: string;
    hookName?: string;
    toolUseID?: string;
    hookEvent?: string;
    content?: string;
    stdout?: string;
    stderr?: string;
    exitCode?: number;
    command?: string;
    durationMs?: number;
    [k: string]: unknown;
  };
}

export interface PermissionModeEvent extends BaseEvent {
  type: "permission-mode";
  permissionMode: string;
}

export interface LastPromptEvent extends BaseEvent {
  type: "last-prompt";
  prompt?: string;
  [k: string]: unknown;
}

export interface QueueOperationEvent extends BaseEvent {
  type: "queue-operation";
  operation?: string;
  [k: string]: unknown;
}

export interface FileHistorySnapshotEvent extends BaseEvent {
  type: "file-history-snapshot";
  snapshot?: unknown;
  [k: string]: unknown;
}

export interface AiTitleEvent extends BaseEvent {
  type: "ai-title";
  title?: string;
}

export interface CustomTitleEvent extends BaseEvent {
  type: "custom-title";
  title?: string;
}

export interface AgentNameEvent extends BaseEvent {
  type: "agent-name";
  name?: string;
}

export interface ProgressEvent extends BaseEvent {
  type: "progress";
  [k: string]: unknown;
}

export interface PrLinkEvent extends BaseEvent {
  type: "pr-link";
  url?: string;
  [k: string]: unknown;
}

export type SessionEvent =
  | UserEvent
  | AssistantEvent
  | SystemEvent
  | AttachmentEvent
  | PermissionModeEvent
  | LastPromptEvent
  | QueueOperationEvent
  | FileHistorySnapshotEvent
  | AiTitleEvent
  | CustomTitleEvent
  | AgentNameEvent
  | ProgressEvent
  | PrLinkEvent
  | (BaseEvent & { [k: string]: unknown });

export type SourceId = "claude" | "codex";

export interface ProjectMeta {
  id: string; // encoded directory name as Claude Code stores it
  decodedPath: string; // best-effort decoded absolute path
  sessionCount: number;
  lastModified: string; // ISO
  source: SourceId; // NEW
}

export interface SessionMeta {
  id: string; // session uuid (no .jsonl extension)
  projectId: string;
  filePath: string;
  title: string;
  firstTimestamp: string | null;
  lastTimestamp: string | null;
  messageCount: number;
  toolUseCount: number;
  hasSubagents: boolean;
  gitBranch?: string;
  cwd?: string;
  fileSize: number;
  isActive: boolean; // mtime within last 5 min
  source: SourceId; // NEW
}

export interface SubagentMeta {
  agentId: string; // e.g. "agent-a4fa7f5f2b14c3d1a"
  filePath: string;
  fileSize: number;
  lastModified: string;
  meta?: Record<string, unknown>;
}

export interface SearchHit {
  projectId: string;
  sessionId: string;
  eventUuid?: string;
  excerpt: string;
  matchType: "text" | "tool_input" | "tool_result" | "thinking" | "title";
  score: number;
  /**
   * How this hit was selected — "filter" when only token filters narrowed
   * the candidates (no free-text needle), or the matching field otherwise.
   * UI surfaces this so the user understands why a session showed up.
   */
  via?: "filter" | "text" | "thinking" | "tool_input" | "tool_result" | "title";
}

export interface DistinctValues {
  tools: Array<{ name: string; count: number }>;
  branches: string[];
  models: string[];
}
