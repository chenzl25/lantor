// Links to Slack messages and channels (`https://<workspace>.slack.com/archives/…`).
// Everything shown comes from the URL itself: Lantor holds no Slack access, so
// it cannot look up a channel's name or a message's text.

export type SlackLink = {
  workspace: string;
  channelId: string;
  /** When the linked message was posted; null for a link to a channel. */
  postedAt: Date | null;
  /** The message is a reply inside a thread. */
  reply: boolean;
};

const SLACK_ARCHIVE_URL = /^https:\/\/([a-z0-9][a-z0-9-]*)\.slack\.com\/archives\/([A-Z0-9]+)(?:\/p(\d{10})(\d{6}))?\/?(?:[?#].*)?$/;

export function parseSlackUrl(href: string | undefined): SlackLink | null {
  const match = href?.match(SLACK_ARCHIVE_URL);
  if (!match || !href) return null;
  const [, workspace, channelId, seconds, micros] = match;
  if (!seconds) return { workspace, channelId, postedAt: null, reply: false };
  // A message link's `p` path segment is its timestamp without the dot; a
  // reply's link also names the thread it belongs to.
  const threadTs = new URL(href).searchParams.get("thread_ts");
  return {
    workspace,
    channelId,
    postedAt: new Date(Number(seconds) * 1000 + Math.floor(Number(micros) / 1000)),
    reply: Boolean(threadTs && threadTs !== `${seconds}.${micros}`),
  };
}

/** The label of a bare Slack URL, which says nothing a reader can use. */
export function slackLinkLabel(link: SlackLink) {
  if (!link.postedAt) return "Slack channel";
  return link.reply ? "Slack reply" : "Slack message";
}
