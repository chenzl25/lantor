import { Slack } from "lucide-react";
import type { AnchorHTMLAttributes, ReactNode } from "react";
import { slackLinkLabel, type SlackLink as SlackLinkTarget } from "../slack-links";
import { formatShortDate, formatTime } from "../ui-utils";

type SlackLinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & {
  link: SlackLinkTarget;
  /** The link text is the URL itself, so it gets a short label instead. */
  bareUrl: boolean;
  children?: ReactNode;
};

// The mark and the first character of the text share a line: a line may
// break after the inline SVG, which would leave the mark alone at a line end.
function splitFirstCharacter(label: ReactNode): [ReactNode, ReactNode] {
  const parts = Array.isArray(label) ? label : [label];
  if (typeof parts[0] !== "string" || !parts[0]) return [null, label];
  const [first, ...others] = Array.from(parts[0]);
  return [first, [others.join(""), ...parts.slice(1)]];
}

/**
 * A link to a Slack message or channel: the Slack mark, the link text (or a
 * short label for a bare URL), and the day the message was posted.
 */
export function SlackLink({ link, bareUrl, children, className, ...props }: SlackLinkProps) {
  const [start, rest] = splitFirstCharacter(bareUrl ? slackLinkLabel(link) : children);
  const posted = link.postedAt?.toISOString();
  const title = [`Slack · ${link.workspace}`, posted && formatTime(posted), link.reply && "reply in a thread"]
    .filter(Boolean).join(" · ");
  return (
    <a {...props} className={["slack-reference", className].filter(Boolean).join(" ")} title={title} target="_blank" rel="noreferrer">
      <span className="slack-reference-start">
        <Slack className="slack-reference-icon" size={12} aria-hidden="true" />
        {start}
      </span>
      {rest}
      {posted && <span className="slack-reference-date">{formatShortDate(posted)}</span>}
    </a>
  );
}
