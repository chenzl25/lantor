import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MarkdownRenderer } from "../src/components/MarkdownRenderer";
import { parseSlackUrl } from "../src/slack-links";

process.env.TZ = "Asia/Singapore";

const base = "https://acme.slack.com/archives/C0EXAMPLE01";
const render = (body: string) => renderToStaticMarkup(createElement(MarkdownRenderer, { body }));
const text = (html: string) => html.replace(/<[^>]+>/g, "");

test("a Slack URL names its workspace, channel, posting time and thread", () => {
  assert.deepEqual(parseSlackUrl(`${base}/p1784310402311459`),
    { workspace: "acme", channelId: "C0EXAMPLE01", postedAt: new Date(1784310402311), reply: false });
  assert.equal(parseSlackUrl(`${base}/p1784310402311459?thread_ts=1784305922.514159&cid=C0EXAMPLE01`)?.reply, true);
  // A thread's own first message links with its own timestamp as the thread.
  assert.equal(parseSlackUrl(`${base}/p1784305922514159?thread_ts=1784305922.514159&cid=C0EXAMPLE01`)?.reply, false);
  assert.deepEqual(parseSlackUrl(base), { workspace: "acme", channelId: "C0EXAMPLE01", postedAt: null, reply: false });
  assert.equal(parseSlackUrl("https://acme.slack.com/team/U0EXAMPLE1"), null);
  assert.equal(parseSlackUrl("https://slack.com.example.com/archives/C0EXAMPLE01"), null);
});

test("a Slack link shows the Slack mark, its text and the day it was posted", () => {
  const html = render(`See [the announcement](${base}/p1784310402311459).`);
  assert.match(html, /<a href="https:\/\/acme\.slack\.com\/archives\/C0EXAMPLE01\/p1784310402311459" class="slack-reference" title="Slack · acme · Jul 18, 01:46 AM" target="_blank"/);
  assert.match(html, /<svg[^>]*slack-reference-icon/);
  assert.match(text(html), /^See the announcementJul 18(, 2026)?\.$/);
});

test("a bare Slack URL becomes a short label", () => {
  // Posted in an earlier year than the test runs, so the day carries it.
  const reply = render(`${base}/p1736913600000100?thread_ts=1736900000.000100&cid=C0EXAMPLE01`);
  assert.match(reply, /title="Slack · acme · Jan 15, 12:00 PM · reply in a thread"/);
  assert.equal(text(reply), "Slack replyJan 15, 2025");
  assert.equal(text(render(`${base}/p1736913600000100`)), "Slack messageJan 15, 2025");
  assert.equal(text(render(base)), "Slack channel");
  assert.doesNotMatch(render(base), /acme\.slack\.com\/archives[^"]*<\/a>|>https:/, "the URL is not on screen");
});

test("a Slack link labelled with its channel keeps the label", () => {
  const html = render(`([#k3-gtm](${base}/p1784310402311459?thread_ts=1784305922.514159&cid=C0EXAMPLE01))`);
  assert.match(text(html), /^\(#k3-gtmJul 18(, 2026)?\)$/);
  assert.equal(html.match(/<a /g)?.length, 1);
});
