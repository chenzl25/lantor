import { memo, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { cachedIdenticon, diceBearAvatarCache, diceBearKey, parseDiceBearAvatar } from "../avatar-rendering";
import type { Agent } from "../types";
import { AgentSubscriptionUsage } from "./AgentSubscriptionUsage";

type AgentAvatarProps = {
  agent: Pick<Agent, "handle" | "display_name" | "status"> &
    Partial<Pick<Agent, "id" | "runtime" | "model" | "role" | "avatar" | "description" | "subscription_status">>;
  size?: "sm" | "md" | "lg";
  className?: string;
  title?: string;
  showStatus?: boolean;
};

type ProfilePopoverPosition = {
  left: number;
  top: number;
  arrowLeft: number;
  maxHeight: number;
  placement: "above" | "below";
};

const PROFILE_POPOVER_WIDTH = 300;
const PROFILE_POPOVER_ESTIMATED_HEIGHT = 320;
const PROFILE_POPOVER_GAP = 10;
const PROFILE_POPOVER_VIEWPORT_MARGIN = 12;
const PROFILE_POPOVER_MEDIA_QUERY = "(hover: hover) and (pointer: fine) and (min-width: 761px)";

function isImageAvatar(value: string) {
  return /^https?:\/\//i.test(value) || /^data:image\//i.test(value);
}

function avatarGlyph(value: string) {
  return Array.from(value).slice(0, 2).join("");
}

function compactProfileText(value: string | null | undefined, fallback = "") {
  const trimmed = value?.trim();
  if (!trimmed) return fallback;
  return trimmed.length > 110 ? `${trimmed.slice(0, 107).trimEnd()}...` : trimmed;
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function getProfilePopoverPosition(
  rect: DOMRect,
  height = PROFILE_POPOVER_ESTIMATED_HEIGHT,
  width = PROFILE_POPOVER_WIDTH,
): ProfilePopoverPosition {
  const viewportWidth = window.innerWidth;
  const viewportHeight = window.innerHeight;
  const maxLeft = Math.max(
    PROFILE_POPOVER_VIEWPORT_MARGIN,
    viewportWidth - width - PROFILE_POPOVER_VIEWPORT_MARGIN,
  );
  const anchorCenter = rect.left + rect.width / 2;
  const left = clamp(
    anchorCenter - width / 2,
    PROFILE_POPOVER_VIEWPORT_MARGIN,
    maxLeft,
  );
  const arrowLeft = clamp(anchorCenter - left, 18, width - 18);
  const roomAbove = rect.top - PROFILE_POPOVER_GAP - PROFILE_POPOVER_VIEWPORT_MARGIN;
  const roomBelow = viewportHeight - rect.bottom - PROFILE_POPOVER_GAP - PROFILE_POPOVER_VIEWPORT_MARGIN;
  const placement = roomAbove >= height || roomAbove >= roomBelow ? "above" : "below";
  const maxHeight = Math.max(0, placement === "above" ? roomAbove : roomBelow);
  const visibleHeight = Math.min(height, maxHeight);
  const preferredTop = placement === "above"
    ? rect.top - PROFILE_POPOVER_GAP - visibleHeight
    : rect.bottom + PROFILE_POPOVER_GAP;
  const maxTop = Math.max(
    PROFILE_POPOVER_VIEWPORT_MARGIN,
    viewportHeight - visibleHeight - PROFILE_POPOVER_VIEWPORT_MARGIN,
  );
  const top = clamp(preferredTop, PROFILE_POPOVER_VIEWPORT_MARGIN, maxTop);
  return { left, top, arrowLeft, maxHeight, placement };
}

function canOpenProfilePopover() {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia(PROFILE_POPOVER_MEDIA_QUERY).matches;
}

function useProfilePopoverEnabled() {
  const [enabled, setEnabled] = useState(canOpenProfilePopover);

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;

    const mediaQuery = window.matchMedia(PROFILE_POPOVER_MEDIA_QUERY);
    const syncEnabled = () => setEnabled(mediaQuery.matches);

    syncEnabled();
    mediaQuery.addEventListener("change", syncEnabled);
    return () => {
      mediaQuery.removeEventListener("change", syncEnabled);
    };
  }, []);

  return enabled;
}

export function AgentAvatar(props: AgentAvatarProps) {
  const { agent } = props;
  // Real agents carry the subscription field even before their first snapshot.
  // Owner avatars and form previews do not have provider usage to display.
  const hasSubscription = "subscription_status" in agent
    && (agent.runtime === "codex" || agent.runtime === "claude")
    && agent.status !== "deleted";
  return hasSubscription ? <AgentAvatarWithProfile {...props} /> : <AgentAvatarImage {...props} />;
}

const AgentAvatarImage = memo(function AgentAvatarImage({ agent, size = "md", className = "", title, showStatus = true }: AgentAvatarProps) {
  const seedText = agent.id || `${agent.handle}:${agent.display_name}:${agent.runtime ?? ""}`;
  const identicon = useMemo(() => cachedIdenticon(seedText), [seedText]);
  const customAvatar = agent.avatar?.trim();
  const diceBearSpec = useMemo(() => customAvatar ? parseDiceBearAvatar(customAvatar, seedText) : null, [customAvatar, seedText]);
  const specKey = diceBearSpec ? diceBearKey(diceBearSpec) : null;
  const imageAvatar = Boolean(customAvatar && !diceBearSpec && isImageAvatar(customAvatar));
  const isDeletedAgent = agent.status === "deleted";
  const [loadedAvatar, setLoadedAvatar] = useState<{ key: string; uri: string } | null>(() => {
    const uri = diceBearSpec && diceBearAvatarCache.get(diceBearSpec);
    return uri && specKey ? { key: specKey, uri } : null;
  });
  const diceBearAvatar = diceBearSpec
    ? diceBearAvatarCache.get(diceBearSpec) ?? (loadedAvatar?.key === specKey ? loadedAvatar.uri : null)
    : null;
  const [failedImageAvatar, setFailedImageAvatar] = useState("");
  const style = {
    "--avatar-color": identicon.foreground,
    "--avatar-bg": identicon.background,
  } as CSSProperties;

  useEffect(() => {
    if (!diceBearSpec) return;
    const cached = diceBearAvatarCache.get(diceBearSpec);
    if (cached) {
      const key = diceBearKey(diceBearSpec);
      // Another row may have resolved the shared request between render/effect.
      setLoadedAvatar((current) => current?.key === key && current.uri === cached ? current : { key, uri: cached });
      return;
    }

    let cancelled = false;
    diceBearAvatarCache.load(diceBearSpec).then((uri) => {
      if (!cancelled) setLoadedAvatar({ key: diceBearKey(diceBearSpec), uri });
    }).catch(() => { /* Keep the identicon fallback; a later mount can retry. */ });

    return () => {
      cancelled = true;
    };
  }, [diceBearSpec]);

  useEffect(() => {
    setFailedImageAvatar("");
  }, [customAvatar]);

  return (
    <span
      className={`avatar agent-avatar agent-avatar-${size} status-${agent.status} ${className}`.trim()}
      data-agent-deleted={isDeletedAgent ? "true" : "false"}
      data-show-status={showStatus ? "true" : "false"}
      style={style}
      title={title}
      aria-hidden={!title}
    >
      {diceBearAvatar ? (
        <img className="agent-avatar-image" src={diceBearAvatar} alt="" aria-hidden="true" />
      ) : customAvatar && imageAvatar && failedImageAvatar !== customAvatar ? (
        <img
          className="agent-avatar-image"
          src={customAvatar}
          alt=""
          aria-hidden="true"
          onError={() => setFailedImageAvatar(customAvatar)}
        />
      ) : customAvatar && !diceBearSpec && !imageAvatar ? (
        <span className="agent-avatar-glyph" aria-hidden="true">{avatarGlyph(customAvatar)}</span>
      ) : (
        <span className="agent-avatar-pixels" aria-hidden="true">
          {identicon.cells.map((filled, index) => (
            <span key={index} className={filled ? "filled" : ""} />
          ))}
        </span>
      )}
    </span>
  );
}, (previous, next) => (
  previous.size === next.size && previous.className === next.className
  && previous.title === next.title && previous.showStatus === next.showStatus
  // Only fields read by this image component matter; subscription/profile
  // refreshes can replace agent objects without changing the avatar.
  && previous.agent.id === next.agent.id && previous.agent.handle === next.agent.handle
  && previous.agent.display_name === next.agent.display_name && previous.agent.runtime === next.agent.runtime
  && previous.agent.avatar === next.agent.avatar && previous.agent.status === next.agent.status
));

export function AgentAvatarWithProfile({
  agent,
  size = "md",
  className = "",
  title,
  showStatus = true,
}: AgentAvatarProps) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const tooltipId = useId();
  const profilePopoverEnabled = useProfilePopoverEnabled();
  const [isProfileOpen, setIsProfileOpen] = useState(false);
  const [profilePosition, setProfilePosition] = useState<ProfilePopoverPosition | null>(null);
  const hasSubscription = (agent.runtime === "codex" || agent.runtime === "claude")
    && agent.status !== "deleted";
  const role = compactProfileText(agent.role);
  const description = compactProfileText(
    agent.status === "deleted" ? "This agent has been deleted." : agent.description,
  );
  const updateProfilePosition = useCallback(() => {
    const rect = anchorRef.current?.getBoundingClientRect();
    if (!rect) return;
    setProfilePosition(getProfilePopoverPosition(
      rect,
      cardRef.current?.offsetHeight,
      cardRef.current?.offsetWidth,
    ));
  }, []);

  useLayoutEffect(() => {
    if (!profilePopoverEnabled || !isProfileOpen) return;
    updateProfilePosition();
    const observer = new ResizeObserver(updateProfilePosition);
    if (cardRef.current) observer.observe(cardRef.current);
    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsProfileOpen(false);
    };
    window.addEventListener("resize", updateProfilePosition);
    window.addEventListener("scroll", updateProfilePosition, true);
    window.addEventListener("keydown", dismissOnEscape);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", updateProfilePosition);
      window.removeEventListener("scroll", updateProfilePosition, true);
      window.removeEventListener("keydown", dismissOnEscape);
    };
  }, [isProfileOpen, profilePopoverEnabled, updateProfilePosition]);

  useEffect(() => {
    if (!profilePopoverEnabled) {
      setIsProfileOpen(false);
      setProfilePosition(null);
    }
  }, [profilePopoverEnabled]);

  useEffect(() => () => {
    if (closeTimerRef.current !== null) clearTimeout(closeTimerRef.current);
  }, []);

  const openProfile = useCallback(() => {
    if (closeTimerRef.current !== null) clearTimeout(closeTimerRef.current);
    if (profilePopoverEnabled) {
      updateProfilePosition();
      setIsProfileOpen(true);
    }
  }, [profilePopoverEnabled, updateProfilePosition]);

  const closeProfile = useCallback(() => {
    if (closeTimerRef.current !== null) clearTimeout(closeTimerRef.current);
    closeTimerRef.current = setTimeout(() => setIsProfileOpen(false), 120);
  }, []);

  const profileCard =
    profilePopoverEnabled && isProfileOpen && profilePosition && typeof document !== "undefined"
      ? createPortal(
          <div
            ref={cardRef}
            id={tooltipId}
            role="tooltip"
            className={`agent-avatar-profile-card agent-avatar-profile-card-visible agent-avatar-profile-card-${profilePosition.placement}`}
            onMouseEnter={openProfile}
            onMouseLeave={closeProfile}
            onClick={(event) => event.stopPropagation()}
            style={
              {
                left: profilePosition.left,
                top: profilePosition.top,
                maxHeight: profilePosition.maxHeight,
                "--agent-avatar-profile-arrow-left": `${profilePosition.arrowLeft}px`,
              } as CSSProperties
            }
          >
            <AgentAvatarImage agent={agent} size="md" className="agent-avatar-profile-image" />
            <span className="agent-avatar-profile-copy">
              <span className="agent-avatar-profile-name">{agent.display_name}</span>
              <span className="agent-avatar-profile-handle">@{agent.handle}</span>
              {!hasSubscription && role ? <span className="agent-avatar-profile-role">{role}</span> : null}
              {!hasSubscription && description ? <span className="agent-avatar-profile-description">{description}</span> : null}
            </span>
            {hasSubscription && <AgentSubscriptionUsage agent={{ ...agent, runtime: agent.runtime ?? "" }} compact />}
          </div>,
          document.body,
        )
      : null;

  return (
    <span
      ref={anchorRef}
      className="agent-avatar-profile-anchor"
      aria-describedby={isProfileOpen ? tooltipId : undefined}
      onMouseEnter={openProfile}
      onMouseLeave={closeProfile}
      onClick={() => setIsProfileOpen(false)}
    >
      <AgentAvatarImage agent={agent} size={size} className={className} showStatus={showStatus} title={profilePopoverEnabled ? undefined : title} />
      {profileCard}
    </span>
  );
}
