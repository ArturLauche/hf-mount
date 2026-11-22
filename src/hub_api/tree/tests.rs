use super::*;
use crate::hub_api::{HubOps, RepoType};
use crate::test_mocks::serve_hub_responses;

#[tokio::test]
async fn repo_listing_and_probe_follow_the_mounted_ref_and_subfolder() {
    let info = r#"{"id":"org/model","sha":"commit-b","lastModified":"2025-01-01T00:00:00Z"}"#;
    let (url, requests) = serve_hub_responses(&[
        ("", info),
        (
            "Link: </previous>; rel=prev\r\nLink: <?cursor=a,b>; title=\"more, files\"; rel=\"next alternate\"\r\n",
            r#"[{"path":"nested/a","type":"file","size":12,"lfs":{"size":1234},"oid":"a"},
             {"path":"nested","type":"directory"},{"path":"nested-sibling","type":"file"}]"#,
        ),
        (
            "Link: <?cursor=first>; rel=prev\r\n",
            r#"[{"path":"nested/b","type":"file","size":42}]"#,
        ),
        ("", info),
    ])
    .await;
    let client = HubApiClient::from_source(
        &url,
        Some("test"),
        None,
        SourceKind::Repo {
            repo_id: "alias".into(),
            repo_type: RepoType::Model,
            revision: "refs/pr/7".into(),
        },
        "nested".into(),
        "test",
    )
    .await
    .unwrap();
    let entries = client.list_tree("").await.unwrap();
    assert_eq!(
        entries.iter().map(|e| (e.path.as_str(), e.size)).collect::<Vec<_>>(),
        [("a", Some(1234)), ("b", Some(42))]
    );
    assert_eq!(entries[0].oid.as_deref(), Some("a"));
    assert_eq!(client.probe_revision().await.unwrap(), "commit-b");

    let requests = requests.await.unwrap();
    let paths: Vec<_> = requests
        .iter()
        .map(|r| r.split_ascii_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "/api/models/alias/revision/refs%2Fpr%2F7?expand=sha&expand=lastModified",
            "/api/models/org/model/tree/refs%2Fpr%2F7/nested?limit=1000",
            "/api/models/org/model/tree/refs%2Fpr%2F7/nested?cursor=a,b",
            "/api/models/org/model/revision/refs%2Fpr%2F7?expand=sha&expand=lastModified",
        ]
    );
    assert!(
        requests
            .iter()
            .all(|r| r.to_ascii_lowercase().contains("authorization: bearer test\r\n"))
    );
}

#[tokio::test]
async fn bucket_continuation_on_a_different_origin_is_anonymous() {
    let (second, forwarded) = serve_hub_responses(&[("", r#"[{"path":"dir/b","type":"file","xetHash":"h2"}]"#)]).await;
    let (url, first) = serve_hub_responses(&[(
        &format!("Link: <{second}/next>; rel=next\r\n"),
        r#"[{"path":"dir/a","type":"file"},{"path":"dir#1","type":"file"}]"#,
    )])
    .await;
    let client = HubApiClient::new(&url, Some("test"), "org/bucket", "test");
    let entries = client.list_tree("dir").await.unwrap();
    assert_eq!(
        entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["dir/a", "dir/b"]
    );
    assert_eq!(entries[1].xet_hash.as_deref(), Some("h2"));
    let first = first.await.unwrap();
    assert!(first[0].starts_with("GET /api/buckets/org/bucket/tree/dir?recursive=false&limit=5000 "));
    assert!(first[0].to_ascii_lowercase().contains("authorization: bearer test\r\n"));
    assert!(
        !forwarded.await.unwrap()[0]
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[tokio::test]
async fn repo_timestamp_is_not_a_commit_and_a_pinned_listing_cannot_switch_back_to_the_branch() {
    let legacy_info = r#"{"id":"org/model","lastModified":"2025-01-01T00:00:00Z"}"#;
    let (url, requests) = serve_hub_responses(&[
        ("", legacy_info),
        ("", legacy_info),
        ("", legacy_info),
        ("", "[]"),
        (
            "Link: </api/models/org/model/tree/main?cursor=2>; rel=next\r\n",
            r#"[{"path":"partial","type":"file"}]"#,
        ),
    ])
    .await;
    let client = HubApiClient::from_source(
        &url,
        None,
        None,
        SourceKind::Repo {
            repo_id: "org/model".into(),
            repo_type: RepoType::Model,
            revision: "main".into(),
        },
        String::new(),
        "test",
    )
    .await
    .unwrap();
    assert_eq!(client.probe_revision().await.unwrap(), "2025-01-01T00:00:00Z");
    assert_eq!(client.probe_repo_commit().await.unwrap(), None);
    let error = client.list_tree_at_commit("", "commit-a").await.unwrap_err();
    assert!(error.to_string().contains("left the pinned commit"));
    let requests = requests.await.unwrap();
    assert!(requests[3].starts_with("GET /api/models/org/model/commits/main?limit=1 "));
    assert!(requests[4].starts_with("GET /api/models/org/model/tree/commit-a?limit=1000 "));
}

#[tokio::test]
async fn pinned_pages_accept_equivalent_paths_but_keep_segment_boundaries() {
    let (url, requests) = serve_hub_responses(&[
        ("", r#"{"id":"org/model","sha":"commit-a"}"#),
        (
            "Link: </api/models/org/model/tree/%63ommit-a/caf%c3%a9/./docs/?cursor=2>; rel=next\r\n",
            "[]",
        ),
        ("", r#"[{"path":"café/docs/item","type":"file"}]"#),
        (
            "Link: </api/models/org/model/tr%65e/commit-a/caf%c3%a9%2fdocs?cursor=2>; rel=next\r\n",
            "[]",
        ),
        (
            "Link: </api/models/org/model/tr%65e/main/caf%c3%a9/docs?cursor=2>; rel=next\r\n",
            "[]",
        ),
    ])
    .await;
    let client = HubApiClient::from_source(
        &url,
        None,
        None,
        SourceKind::Repo {
            repo_id: "org/model".into(),
            repo_type: RepoType::Model,
            revision: "main".into(),
        },
        String::new(),
        "test",
    )
    .await
    .unwrap();
    let entries = client.list_tree_at_commit("café/docs", "commit-a").await.unwrap();
    assert_eq!(entries[0].path, "café/docs/item");
    for _ in 0..2 {
        let err = client.list_tree_at_commit("café/docs", "commit-a").await.unwrap_err();
        assert!(err.to_string().contains("left the pinned commit or directory"), "{err}");
    }
    let requests = requests.await.unwrap();
    assert!(requests[2].contains("/caf%c3%a9/docs/?cursor=2 "));
}

#[tokio::test]
async fn incomplete_or_cyclic_listing_is_an_error() {
    for (headers, error) in [
        ("Link: <#again>; rel=next\r\n", "repeated a pagination link"),
        ("Link: <unfinished; rel=next\r\n", "invalid tree listing Link header"),
        ("Link: <?cursor=2>\r\n", "missing or empty rel"),
        ("Link: <?cursor=2>; rel\r\n", "missing or empty rel"),
        ("Link: <?cursor=2>; rel=\"\"; rel=next\r\n", "missing or empty rel"),
        ("Link: <?cursor=2>; rel=\"  \"\r\n", "missing or empty rel"),
        (
            "Link: <?cursor=2>; rel=next, </unknown>; title=unknown\r\n",
            "missing or empty rel",
        ),
        ("Link: <?cursor=2>; rel=next; anchor\r\n", "missing anchor value"),
    ] {
        let (url, requests) = serve_hub_responses(&[(headers, r#"[{"path":"partial","type":"file"}]"#)]).await;
        let client = HubApiClient::new(&url, None, "org/bucket", "test");
        assert!(client.list_tree("").await.unwrap_err().to_string().contains(error));
        assert_eq!(requests.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn pagination_uses_the_first_relation_and_its_resolved_context() {
    let (url, requests) = serve_hub_responses(&[
        (
            concat!(
                "Link: </wrong1>; rel=prev; rel=next, </wrong2>; REL=next; anchor=\"/other\"\r\n",
                "Link: </wrong3>; rel=next; anchor=\"#section\"\r\n",
                "Link: <?cursor=a,b=c>; title=\"quoted \\\"; rel = prev, = value\"; ",
                "REL \t= \"alternate  NeXt\"; AnChOr = \"\"\r\n"
            ),
            r#"[{"path":"a","type":"file"}]"#,
        ),
        ("Link: <?cursor=end>; rel=next; anchor=\"?cursor=a,b=c\"\r\n", "[]"),
        (
            "Link: <?cursor=wrong>; rel=next; anchor=\"/other\"\r\n",
            r#"[{"path":"b","type":"file"}]"#,
        ),
    ])
    .await;
    let client = HubApiClient::new(&url, None, "org/bucket", "test");
    let entries = client.list_tree("").await.unwrap();
    assert_eq!(entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    let requests = requests.await.unwrap();
    assert!(requests[1].starts_with("GET /api/buckets/org/bucket/tree?cursor=a,b=c "));
    assert!(requests[2].starts_with("GET /api/buckets/org/bucket/tree?cursor=end "));
}

#[tokio::test]
async fn subfolder_validation_requests_one_child_from_the_mounted_revision() {
    let (url, requests) = serve_hub_responses(&[
        ("", r#"{"id":"org/data"}"#),
        (
            "Link: <?cursor=more>; rel=next\r\n",
            r#"[{"path":"é/data/one","type":"file"}]"#,
        ),
    ])
    .await;
    let client = HubApiClient::from_source(
        &url,
        None,
        None,
        SourceKind::Repo {
            repo_id: "org/data".into(),
            repo_type: RepoType::Dataset,
            revision: "feature".into(),
        },
        "é/data".into(),
        "test",
    )
    .await
    .unwrap();
    client.validate_path_prefix().await.unwrap();
    assert!(requests.await.unwrap()[1].starts_with("GET /api/datasets/org/data/tree/feature/%C3%A9/data?limit=1 "));
}

#[tokio::test]
async fn existence_probe_skips_raw_prefix_matches_and_stops_at_a_real_child() {
    let siblings = r#"[{"path":"doc","type":"directory"},{"path":"doc#1","type":"file"}]"#;
    let (url, requests) = serve_hub_responses(&[
        ("Link: <?cursor=2>; rel=next\r\n", siblings),
        (
            "Link: <?cursor=3>; rel=next\r\n",
            r#"[{"path":"doc/file","type":"file"}]"#,
        ),
        ("", siblings),
    ])
    .await;
    let client = HubApiClient::new(&url, None, "org/bucket", "test");
    assert!(client.has_children("doc").await.unwrap());
    assert!(!client.has_children("doc").await.unwrap());
    let requests = requests.await.unwrap();
    assert!(requests[1].starts_with("GET /api/buckets/org/bucket/tree/doc?cursor=2 "));
    assert!(requests[2].starts_with("GET /api/buckets/org/bucket/tree/doc?recursive=false&limit=5000 "));
}
