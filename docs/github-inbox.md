# GitHub inbox

The **GitHub** panel (sidebar, or the bottom bar on phones) lists the owner's
GitHub notifications from every repository, not only the ones bound to
channels. It mirrors github.com/notifications: the filters are Participating,
Review requested, Assigned, Mentioned and Team mentioned, and each filter's
count is its number of unread threads, the same number GitHub shows. The
badge on the sidebar row and on the phone's bottom-bar tab counts unread
participating threads.

## Sync

`github/inbox.rs` reads the GitHub notifications API through `gh`, with the
account `gh` is logged in to:

- `notifications?participating=true`, every page (up to 500 threads): all
  unread threads, so the counts match GitHub's.
- `notifications?participating=true&all=true&per_page=50`: the newest threads,
  read or not, so read ones stay listed as on GitHub.

The union replaces `github_inbox_threads`; threads in neither answer are
dropped. `gh api user` is checked once an hour; another login clears the
threads. A sync publishes the `github_inbox_updated` refresh only when the list,
the account or the error changed, so a quiet minute costs no client reloads.

The inbox is off until the owner first opens the panel: that open runs
`refresh_github_inbox`, which turns it on. From then on the process that serves
the API (desktop app or `--web-only`, not the supervisor) polls once a minute,
GitHub's requested interval, and five minutes after a failure. Opening the
panel also syncs when the last sync is over 30 seconds old. A failure keeps the
cached threads and shows the error in the panel.

## List

Unread threads come first, then read ones, each newest first. A thread read
while the list is open keeps its place until the list is shown again (the
panel reopens, or the channel tab comes back to Notifications), so reading a
row does not move the rows around it (`useGithubInboxOrder`). An unread thread
has a dot left of its type icon. Once the pull request or issue state is known
(`useGithubReferenceItem`), the type icon shows it in GitHub's colors.

## Read and done

Opening a thread opens its GitHub page, at its latest comment when GitHub names
one, and marks it read. **Done**, the row's only button, marks it done; with a
mouse it shows on the row under the pointer, as on GitHub. Both write to GitHub
first (`PATCH` / `DELETE notifications/threads/{id}`) and change nothing
locally when that fails. GitHub has no API to mark a thread unread.

Locally the mark keeps the thread's GitHub `updated_at` (`read_updated_at`,
`done_updated_at`). A sync that started before GitHub took the write still
reports the thread unread with the same `updated_at`, so the mark holds; new
activity raises `updated_at`, and the thread is unread (or back from done)
again, as on GitHub.

## Lantor threads

A pull request or issue with a review or issue task in a channel bound to its
repository (`github_resource_threads`) shows **Open Lantor thread**. A pull
request without one, in a bound repository, shows **Review with agent**: it
starts the same review task as the channel's GitHub tab, in that channel.

## Channels

A thread in a repository bound to a channel names that channel (`channel_id`;
with several, the one that reviews as the inbox's account, then the oldest).
The channel chip opens the channel's GitHub tab at the thread.

Once the inbox is on for the account a channel reviews as
(`github_unread_from_inbox`), the inbox is that channel's GitHub unread state:

- The GitHub tab's badge counts the unread pull request and issue threads of
  the bound repository (`github_unread_count` in `channels.rs`). Opening the
  tab no longer clears it; reading a thread on either side does, on GitHub
  too. Until then, and for a channel that reviews as another login, the badge
  counts review requests not seen yet and clears when the tab opens.
- The tab's **Notifications** view lists those threads with the inbox's row
  and order, and **Mark all read**, which writes each unread one to GitHub
  in turn (`mark_github_inbox_repository_read`). A thread GitHub took stays
  read when a later one fails.
- Pull request and issue cards with an unread thread show its reason; opening
  one marks the thread read.

A sync reports the repositories whose pull request or issue threads are new or
updated. Each channel bound to one refreshes that queue in the background
(pull requests, issues, or both), at most every five minutes per queue: a full
pull request refresh of a busy repository costs about a hundred GraphQL points.
Each refresh publishes `github_channel_queue_refreshed`, and an open tab
reloads when the channel's `github_review_synced_at` or
`github_issue_synced_at` changes.

## Data path

`github_inbox` is a bootstrap collection and a `load_ui_state` scope. The
`github_inbox_updated` refresh reloads it and `channels`, whose GitHub badges
count it; `github_repository_bound` does the same, and a new review or issue
task also reloads the inbox for its thread link. The panel's actions return the
stored inbox, so the panel updates without waiting for the event.

`npm run test:github-inbox` checks the panel, and a bound channel's GitHub
tab, in Chromium, WebKit and an iPhone viewport against a synthetic API;
`cargo test github::inbox` covers sync, marks, channel counts, background
refresh planning, failures and account changes with a fake `gh`.
