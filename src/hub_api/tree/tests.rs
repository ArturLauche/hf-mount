use super::*;
use crate::hub_api::RepoType;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Serve one JSON response per request and record the actual HTTP request lines and headers.
async fn server(pages: &[(&str, &str)]) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let responses: Vec<_> = pages
        .iter()
        .map(|(headers, body)| {
            format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                body.len()
            )
        })
        .collect();
    let requests = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (socket, _) = listener.accept().await.unwrap();
            let mut input = BufReader::new(socket);
            let mut request = String::new();
            while !request.ends_with("\r\n\r\n") {
                assert_ne!(input.read_line(&mut request).await.unwrap(), 0);
            }
            requests.push(request);
            input.into_inner().write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, requests)
}

#[tokio::test]
async fn repo_listing_and_probe_follow_the_mounted_ref_and_subfolder() {
    let info = r#"{"id":"org/model","sha":"commit-b","lastModified":"2025-01-01T00:00:00Z"}"#;
    let (url, requests) = server(&[
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
    let (second, forwarded) = server(&[("", r#"[{"path":"dir/b","type":"file","xetHash":"h2"}]"#)]).await;
    let (url, first) = server(&[(
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
async fn incomplete_or_cyclic_listing_is_an_error() {
    for (headers, error) in [
        ("Link: <#again>; rel=next\r\n", "repeated a pagination link"),
        ("Link: <unfinished; rel=next\r\n", "invalid tree listing Link header"),
    ] {
        let (url, requests) = server(&[(headers, r#"[{"path":"partial","type":"file"}]"#)]).await;
        let client = HubApiClient::new(&url, None, "org/bucket", "test");
        assert!(client.list_tree("").await.unwrap_err().to_string().contains(error));
        assert_eq!(requests.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn subfolder_validation_requests_one_child_from_the_mounted_revision() {
    let (url, requests) = server(&[
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
    let (url, requests) = server(&[
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
