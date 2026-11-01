use super::*;
use crate::test_support::{drop_test_schema, test_pool};

async fn insert_binding(pool: &SqlitePool, channel: &str, repository_id: &str, name: &str) -> Uuid {
    let channel_id: Uuid =
        sqlx::query_scalar("insert into channels (name) values ($1) returning id")
            .bind(channel)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query(
        r#"
        insert into channel_github_repositories (
            channel_id, repository_id, name_with_owner, url, account_login, review_login
        )
        values ($1, $2, $3, $4, 'me', 'me')
        "#,
    )
    .bind(channel_id)
    .bind(repository_id)
    .bind(name)
    .bind(format!("https://github.com/{name}"))
    .execute(pool)
    .await
    .unwrap();
    channel_id
}

async fn insert_pull(
    pool: &SqlitePool,
    channel_id: Uuid,
    number: i64,
    title: &str,
    updated_at: &str,
) {
    sqlx::query(
        r#"
        insert into github_review_request_cache (
            channel_id, repository_id, review_login, pull_number, title, url, author_login,
            is_draft, state, github_updated_at, checks_status
        )
        select $1, repository_id, $2, $3, $4, url || '/pull/' || $3, 'author', 1, 'OPEN', $5, 'failure'
        from channel_github_repositories
        where channel_id = $1
        "#,
    )
    .bind(channel_id)
    .bind(format!("login-{updated_at}"))
    .bind(number)
    .bind(title)
    .bind(updated_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn references_merge_bindings_and_cached_items() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let main = insert_binding(&pool, "main", "R_1", "org/main-repo").await;
    let mirror = insert_binding(&pool, "mirror", "R_1", "org/main-repo").await;
    let other = insert_binding(&pool, "other", "R_2", "org/other").await;
    insert_pull(&pool, main, 7, "older title", "2026-01-01T00:00:00Z").await;
    insert_pull(&pool, mirror, 7, "newer title", "2026-01-02T00:00:00Z").await;
    sqlx::query(
        r#"
        insert into github_issue_cache (
            channel_id, repository_id, queue_login, issue_number, title, url, author_login,
            state, created_at, github_updated_at, comments_count, labels_json, assignees_json,
            is_related
        )
        values ($1, 'R_2', 'me', 3, 'an issue', 'https://github.com/org/other/issues/3',
                'author', 'open', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, '[]', '[]', 1)
        "#,
    )
    .bind(other)
    .execute(&pool)
    .await
    .unwrap();

    let references = load_github_references(&pool).await.unwrap();
    assert_eq!(references.repositories.len(), 3);
    assert!(references
        .repositories
        .iter()
        .all(|repository| repository.reference_alias.is_empty()));
    assert_eq!(references.items.len(), 2);
    let pull = &references.items[0];
    assert_eq!(
        (
            pull.name_with_owner.as_str(),
            pull.number,
            pull.kind,
            pull.title.as_str()
        ),
        ("org/main-repo", 7, "pull", "newer title"),
    );
    assert_eq!(
        (
            pull.state.as_str(),
            pull.is_draft,
            pull.checks_status.as_str()
        ),
        ("open", true, "failure"),
    );
    let issue = &references.items[1];
    assert_eq!(
        (issue.number, issue.kind, issue.is_draft),
        (3, "issue", false)
    );

    let references = set_github_reference_alias(&pool, main, " RW# ")
        .await
        .unwrap();
    // Bindings of one repository are ordered by (random) channel id.
    let alias_of = |channel_id: Uuid| {
        references
            .repositories
            .iter()
            .find(|repository| repository.channel_id == channel_id)
            .map(|repository| repository.reference_alias.as_str())
    };
    assert_eq!(
        [alias_of(main), alias_of(mirror), alias_of(other)],
        [Some("rw"), Some(""), Some("")]
    );
    // Another channel bound to the same repository may share the alias.
    set_github_reference_alias(&pool, mirror, "rw")
        .await
        .unwrap();
    let error = set_github_reference_alias(&pool, other, "rw")
        .await
        .unwrap_err();
    assert!(error.contains("org/main-repo"), "{error}");
    assert!(set_github_reference_alias(&pool, other, "task")
        .await
        .is_err());
    assert!(set_github_reference_alias(&pool, other, "9x")
        .await
        .is_err());
    set_github_reference_alias(&pool, main, "").await.unwrap();
    let unbound: Uuid =
        sqlx::query_scalar("insert into channels (name) values ('plain') returning id")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(set_github_reference_alias(&pool, unbound, "pl")
        .await
        .is_err());
    drop_test_schema(pool, schema).await;
}

fn binding(channel_id: Uuid, name_with_owner: &str, alias: &str) -> GithubReferenceRepository {
    GithubReferenceRepository {
        channel_id,
        name_with_owner: name_with_owner.to_owned(),
        url: format!("https://github.com/{name_with_owner}"),
        reference_alias: alias.to_owned(),
    }
}

#[test]
fn agent_reference_lines_name_the_channel_repository_and_short_names() {
    let rw = Uuid::from_u128(1);
    let rw_again = Uuid::from_u128(2);
    let lantor = Uuid::from_u128(3);
    let other = Uuid::from_u128(4);
    let bindings = vec![
        binding(lantor, "chenzl25/lantor", ""),
        binding(other, "acme/iceberg-rust", ""),
        binding(Uuid::from_u128(5), "risingwavelabs/iceberg-rust", ""),
        binding(rw, "risingwavelabs/risingwave", ""),
        binding(rw_again, "risingwavelabs/risingwave", "rw"),
        // An alias wins over another repository's name.
        binding(
            Uuid::from_u128(6),
            "nimtable/iceberg-compaction",
            "risingwave-docs",
        ),
        binding(Uuid::from_u128(7), "risingwavelabs/risingwave-docs", ""),
        binding(Uuid::from_u128(8), "someone/task", ""),
    ];

    let lines = agent_github_reference_lines(&bindings, Some(lantor));
    assert_eq!(
        lines,
        vec![
            "github_repo: chenzl25/lantor (a bare #123 here links to it)".to_owned(),
            "github_names: acme/iceberg-rust, lantor=chenzl25/lantor, \
             risingwave-docs=nimtable/iceberg-compaction, risingwavelabs/iceberg-rust, \
             rw=risingwavelabs/risingwave, risingwavelabs/risingwave-docs, someone/task"
                .to_owned(),
        ]
    );

    let unbound = agent_github_reference_lines(&bindings, Some(Uuid::from_u128(99)));
    assert_eq!(
        unbound[0],
        "github_repo: none (a bare #123 here is not linked)"
    );
    assert_eq!(agent_github_reference_lines(&bindings, None)[0], unbound[0]);
    assert!(agent_github_reference_lines(&[], Some(lantor)).is_empty());
}

#[tokio::test]
async fn agent_reference_lines_read_channel_bindings() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let main = insert_binding(&pool, "main", "R_1", "org/main-repo").await;
    insert_binding(&pool, "side", "R_2", "org/side-repo").await;
    set_github_reference_alias(&pool, main, "mr").await.unwrap();

    let lines = load_agent_github_reference_lines(&pool, Some(main))
        .await
        .unwrap();
    assert_eq!(
        lines,
        vec![
            "github_repo: org/main-repo (a bare #123 here links to it)".to_owned(),
            "github_names: mr=org/main-repo, side-repo=org/side-repo".to_owned(),
        ]
    );

    drop_test_schema(pool, schema).await;
}
