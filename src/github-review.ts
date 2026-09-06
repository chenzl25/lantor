import type { GithubChannelOverview, GithubReviewComparisons } from "./types";

export function githubComparisonRequestKey(overview: GithubChannelOverview): string | null {
  if (!overview.binding) return null;
  const missing = overview.review_requests
    .filter((pull) => pull.review_is_stale && pull.review_commits_ahead === null)
    .map((pull) => [pull.number, pull.review_anchor_sha, pull.head_sha])
    .sort((a, b) => Number(a[0]) - Number(b[0]));
  if (missing.length === 0) return null;
  return JSON.stringify([overview.binding.channel_id, overview.binding.repository_id, missing]);
}

export function mergeGithubReviewComparisons(
  overview: GithubChannelOverview,
  result: GithubReviewComparisons,
): GithubChannelOverview {
  if (overview.binding?.repository_id !== result.repository_id) return overview;
  const counts = new Map(result.comparisons.map((comparison) => [comparison.pull_number, comparison]));
  let changed = false;
  const reviewRequests = overview.review_requests.map((pull) => {
    const comparison = counts.get(pull.number);
    // A refresh, new push, or Re-review can finish before these optional counts.
    if (!comparison || !pull.review_is_stale
      || comparison.review_anchor_sha !== pull.review_anchor_sha
      || comparison.head_sha !== pull.head_sha
      || comparison.commits_ahead === pull.review_commits_ahead) return pull;
    changed = true;
    return { ...pull, review_commits_ahead: comparison.commits_ahead };
  });
  return changed ? { ...overview, review_requests: reviewRequests } : overview;
}
