import { Activity, ArrowLeft, Bot, CalendarDays, ChevronDown, FileText, Hash, LayoutList, MessageSquare, Search, X } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import type { Agent, OwnerProfile, SearchResult, SearchScope, SearchTimeRange } from "../types";
import { APP_DISPLAY_NAME } from "../branding";
import { ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

type SearchModalProps = {
  open: boolean;
  query: string;
  scope: SearchScope;
  timeRange: SearchTimeRange;
  results: SearchResult[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  onQueryChange: (value: string) => void;
  onScopeChange: (value: SearchScope) => void;
  onTimeRangeChange: (value: SearchTimeRange) => void;
  onOpenResult: (result: SearchResult) => void;
  onClear: () => void;
  onClose: () => void;
};

const SCOPE_OPTIONS: Array<{ value: SearchScope; label: string }> = [
  { value: "all", label: "All" },
  { value: "messages", label: "Messages" },
  { value: "channels", label: "Channels & DMs" },
  { value: "tasks", label: "Tasks" },
  { value: "agents", label: "Agents" },
  { value: "activity", label: "Activity" },
  { value: "artifacts", label: "Artifacts" },
];

const TIME_OPTIONS: Array<{ value: SearchTimeRange; label: string }> = [
  { value: "any", label: "Any time" },
  { value: "today", label: "Today" },
  { value: "7d", label: "Last 7 days" },
  { value: "30d", label: "Last 30 days" },
];

const GROUPS = [
  { key: "channels", title: "Channels & DMs", kinds: new Set(["channel", "dm"]), icon: Hash },
  { key: "messages", title: "Messages", kinds: new Set(["message", "reply"]), icon: MessageSquare },
  { key: "tasks", title: "Tasks", kinds: new Set(["task"]), icon: LayoutList },
  { key: "agents", title: "Agents", kinds: new Set(["agent"]), icon: Bot },
  { key: "activity", title: "Activity & agent turns", kinds: new Set(["activity", "request"]), icon: Activity },
  { key: "artifacts", title: "Artifacts", kinds: new Set(["artifact"]), icon: FileText },
];

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function resultLabel(kind: string) {
  if (kind === "dm") return "DM";
  if (kind === "reply") return "Thread";
  if (kind === "request") return "Agent turn";
  if (kind === "artifact") return "Artifact";
  return kind;
}

function Highlight({ text, query }: { text: string; query: string }) {
  const needle = query.trim();
  if (!needle) return <>{text}</>;
  const pattern = new RegExp(`(${escapeRegExp(needle)})`, "ig");
  return (
    <>
      {text.split(pattern).map((part, index) =>
        part.toLowerCase() === needle.toLowerCase()
          ? <mark key={`${part}-${index}`}>{part}</mark>
          : <span key={`${part}-${index}`}>{part}</span>,
      )}
    </>
  );
}

function resultAvatarAgent(result: SearchResult, agents: Agent[], ownerProfile: OwnerProfile) {
  if (result.agentId) return agents.find((agent) => agent.id === result.agentId) ?? null;
  if (result.senderRole === "owner") return ownerAsAvatarAgent(ownerProfile);
  return null;
}

export function SearchModal({
  open,
  query,
  scope,
  timeRange,
  results,
  agents,
  ownerProfile,
  onQueryChange,
  onScopeChange,
  onTimeRangeChange,
  onOpenResult,
  onClear,
  onClose,
}: SearchModalProps) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const groupedResults = useMemo(() => {
    return GROUPS.map((group) => ({
      ...group,
      results: results.filter((result) => group.kinds.has(result.kind)),
    })).filter((group) => group.results.length > 0);
  }, [results]);

  useEffect(() => {
    if (!open) return;
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus());
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("keydown", onKey);
    };
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="search-backdrop" onClick={onClose}>
      <section className="search-panel" onClick={(event) => event.stopPropagation()}>
        <header className="search-panel-head">
          <div className="search-input-icon"><Search size={24} /></div>
          <input
            ref={inputRef}
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder="Search channels, DMs, messages, tasks, agents..."
          />
          {query.trim() && (
            <button className="search-clear" onClick={onClear} aria-label="Clear search">
              <X size={18} />
            </button>
          )}
          <button className="search-clear search-modal-close" onClick={onClose} aria-label="Close search">
            <X size={18} />
          </button>
        </header>

        <div className="search-filters">
          <button className="search-mobile-top-back" onClick={onClose} aria-label="Close search">
            <ArrowLeft size={19} />
          </button>
          <div className="search-filter-group" role="group" aria-label="Search type">
            {SCOPE_OPTIONS.map((option) => (
              <button
                key={option.value}
                className={scope === option.value ? "active" : ""}
                onClick={() => onScopeChange(option.value)}
              >
                {option.label}
              </button>
            ))}
          </div>
          <label className="search-time-filter">
            <CalendarDays size={15} />
            <select value={timeRange} onChange={(event) => onTimeRangeChange(event.target.value as SearchTimeRange)}>
              {TIME_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>{option.label}</option>
              ))}
            </select>
            <ChevronDown className="search-time-chevron" size={15} aria-hidden="true" />
          </label>
        </div>

        <div className="search-panel-body">
          {!query.trim() && (
            <div className="search-empty">
              <Search size={32} />
              <h3>Search {APP_DISPLAY_NAME}</h3>
              <p>Use filters to narrow by message, channel, task, agent, agent turn, or activity.</p>
            </div>
          )}

          {query.trim() && groupedResults.length === 0 && (
            <div className="search-empty">
              <Search size={32} />
              <h3>No results</h3>
              <p>Try a broader type filter or a longer time range.</p>
            </div>
          )}

          {groupedResults.map((group) => {
            const Icon = group.icon;
            return (
              <section key={group.key} className="search-result-group">
                <h4><Icon size={15} /> {group.title}</h4>
                <div className="search-result-list">
                  {group.results.map((result) => {
                    const avatarAgent = resultAvatarAgent(result, agents, ownerProfile);
                    return (
                      <button key={`${result.kind}-${result.id}`} onClick={() => onOpenResult(result)}>
                        <span className="search-result-avatar" aria-hidden="true">
                          {avatarAgent ? (
                            <AgentAvatar agent={avatarAgent} size="md" showStatus={false} />
                          ) : (
                            <span className="search-result-fallback-avatar">{resultLabel(result.kind).slice(0, 1)}</span>
                          )}
                        </span>
                        <div>
                          <div className="search-result-meta">
                            <strong><Highlight text={result.title} query={query} /></strong>
                            <span>{resultLabel(result.kind)}</span>
                            <small>{result.detail}</small>
                          </div>
                          {result.excerpt && <p><Highlight text={result.excerpt} query={query} /></p>}
                        </div>
                      </button>
                    );
                  })}
                </div>
              </section>
            );
          })}
        </div>
      </section>
    </div>
  );
}
