# GitHub inbox

The **GitHub** panel (sidebar, or the menu on Home on phones) lists the owner's
GitHub notifications from every repository, not only the ones bound to
channels. It mirrors github.com/notifications: the filters are Participating,
Review requested, Assigned, Mentioned and Team mentioned, and each filter's
count is its number of unread threads, the same number GitHub shows. The
sidebar badge counts unread participating threads.

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

## Read and done

Opening a thread opens its GitHub page, at its latest comment when GitHub names
one, and marks it read. **Done** marks it done. Both write to GitHub first
(`PATCH` / `DELETE notifications/threads/{id}`) and change nothing locally when
that fails. GitHub has no API to mark a thread unread.

Locally the mark keeps the thread's GitHub `updated_at` (`read_updated_at`,
`done_updated_at`). A sync that started before GitHub took the write still
reports the thread unread with the same `updated_at`, so the mark holds; new
activity raises `updated_at`, and the thread is unread (or back from done)
again, as on GitHub.

## Lantor threads

A pull request or issue with a review or issue task in a channel bound to its
repository (`github_resource_threads`) shows **Open Lantor thread**.

## Data path

`github_inbox` is a bootstrap collection and a `load_ui_state` scope; the
`github_inbox_updated` refresh reloads only that scope. The panel's actions
return the stored inbox, so the panel updates without waiting for the event.

`npm run test:github-inbox` checks the panel in Chromium, WebKit and an iPhone
viewport against a synthetic API; `cargo test github::inbox` covers sync,
marks, failures and account changes with a fake `gh`.
