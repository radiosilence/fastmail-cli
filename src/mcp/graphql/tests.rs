//! Tests for nested resolution and DataLoader batching.
//!
//! These run the real schema against a mock JMAP server and assert on the
//! *number and shape of the calls made*, which is the whole point of the
//! DataLoader layer — a resolver-level test would pass even with N+1 behaviour.

use std::sync::Arc;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{SharedClient, build_schema, request};
use crate::jmap::JmapClient;

/// One `Email/get`-shaped invocation recorded from a request body.
struct Call {
    method: String,
    ids: Vec<String>,
    properties: Vec<String>,
}

/// Pull every method call out of the recorded JMAP request bodies, in order.
async fn calls(server: &MockServer) -> Vec<Call> {
    let mut out = Vec::new();
    for req in server.received_requests().await.unwrap_or_default() {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let Some(method_calls) = body.get("methodCalls").and_then(Value::as_array) else {
            continue;
        };
        for mc in method_calls {
            let name = mc.get(0).and_then(Value::as_str).unwrap_or("").to_string();
            let args = mc.get(1).cloned().unwrap_or(Value::Null);
            let str_list = |key: &str| -> Vec<String> {
                args.get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            out.push(Call {
                method: name,
                ids: str_list("ids"),
                properties: str_list("properties"),
            });
        }
    }
    out
}

fn email_json(id: &str, thread: &str, with_body: bool) -> Value {
    // Ordering matters (threads sort oldest-first), so derive a distinct
    // timestamp from the numeric suffix of the id rather than a constant.
    let n: u32 = id.trim_start_matches('e').parse().unwrap_or(0);
    let mut e = json!({
        "id": id,
        "blobId": format!("blob-{id}"),
        "threadId": thread,
        "mailboxIds": { "mb1": true },
        "keywords": {},
        "size": 1234,
        "receivedAt": format!("2024-01-{:02}T00:00:00Z", n + 1),
        "subject": format!("Subject {id}"),
        "from": [{ "name": "Sender", "email": "sender@example.com" }],
        "preview": "preview text",
        // Present in the summary property set, so a list result already knows
        // an attachment exists without fetching its metadata.
        "hasAttachment": true,
    });
    if with_body {
        e["textBody"] = json!([{ "partId": "1", "type": "text/plain" }]);
        e["bodyValues"] = json!({ "1": { "value": format!("Body of {id}") } });
        e["attachments"] = json!([{
            "partId": "2",
            "blobId": format!("blob-att-{id}"),
            "name": "notes.txt",
            "type": "text/plain",
            "size": ATTACHMENT_BYTES.len(),
            "charset": "utf-8",
            "disposition": "attachment",
            "cid": format!("cid-{id}"),
        }]);
    }
    e
}

/// Body served for every blob download in these tests.
const ATTACHMENT_BYTES: &[u8] = b"attachment payload";

/// Blob downloads are plain GETs, not JMAP method calls, so they are counted
/// separately from [`calls`] — this is what the laziness tests assert on.
async fn downloads(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::GET)
        .map(|r| r.url.path().to_string())
        .filter(|p| p.contains("/download/"))
        .collect()
}

/// Mock JMAP endpoint that answers `Mailbox/get`, `Email/query`, `Email/get`
/// and `Thread/get` for a fixed inbox of `count` emails.
async fn mock_server(count: usize) -> MockServer {
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..count).map(|i| format!("e{i}")).collect();

    let responder = move |req: &wiremock::Request| -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let empty = vec![];
        let method_calls = body
            .get("methodCalls")
            .and_then(Value::as_array)
            .unwrap_or(&empty);

        let mut responses = Vec::new();
        for mc in method_calls {
            let name = mc.get(0).and_then(Value::as_str).unwrap_or("");
            let args = mc.get(1).cloned().unwrap_or(Value::Null);
            let tag = mc.get(2).and_then(Value::as_str).unwrap_or("c0");

            let payload = match name {
                "Mailbox/get" => json!({ "list": [{
                    "id": "mb1", "name": "Inbox", "role": "inbox",
                    "totalEmails": 10, "unreadEmails": 2,
                    "totalThreads": 8, "unreadThreads": 2, "sortOrder": 0
                }] }),
                "Email/query" => {
                    let total = ids.len() as i64;
                    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(total) as usize;

                    // Resolve the window start the way a JMAP server would:
                    // anchor + offset when given, otherwise position, where a
                    // negative value counts back from the end.
                    let start: i64 = match args.get("anchor").and_then(Value::as_str) {
                        Some(anchor) => {
                            let Some(idx) = ids.iter().position(|i| i == anchor) else {
                                return ResponseTemplate::new(200).set_body_json(json!({
                                    "methodResponses": [[
                                        "error",
                                        { "type": "anchorNotFound",
                                          "description": "anchor not in results" },
                                        tag
                                    ]]
                                }));
                            };
                            idx as i64
                                + args
                                    .get("anchorOffset")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(0)
                        }
                        None => {
                            let p = args.get("position").and_then(Value::as_i64).unwrap_or(0);
                            if p < 0 { total + p } else { p }
                        }
                    };
                    let start = start.max(0) as usize;
                    let window: Vec<&String> = ids.iter().skip(start).take(limit).collect();
                    let mut payload =
                        json!({ "ids": window, "position": start, "queryState": "qs-1" });
                    if args.get("calculateTotal").and_then(Value::as_bool) == Some(true) {
                        payload["total"] = json!(ids.len());
                    }
                    payload
                }
                "Email/get" => {
                    // A back-reference (`#ids`) means this is the list fetch;
                    // an explicit `ids` array means a targeted (batched) fetch.
                    let requested: Vec<String> = match args.get("ids").and_then(Value::as_array) {
                        Some(a) => a
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        None => ids.clone(),
                    };
                    let full = args
                        .get("properties")
                        .and_then(Value::as_array)
                        .is_some_and(|p| p.iter().any(|v| v.as_str() == Some("textBody")));
                    json!({
                        "list": requested.iter()
                            .map(|id| email_json(id, "t1", full))
                            .collect::<Vec<_>>(),
                        "notFound": []
                    })
                }
                "Thread/get" => json!({ "list": [{ "id": "t1", "emailIds": ids }] }),
                _ => json!({ "list": [], "notFound": [] }),
            };
            responses.push(json!([name, payload, tag]));
        }

        ResponseTemplate::new(200).set_body_json(json!({ "methodResponses": responses }))
    };

    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(responder)
        .mount(&server)
        .await;

    // Blob downloads: one GET per blob, which the laziness tests count.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ATTACHMENT_BYTES))
        .mount(&server)
        .await;

    server
}

fn client_for(server: &MockServer) -> SharedClient {
    Arc::new(tokio::sync::Mutex::new(JmapClient::with_test_session(
        &format!("{}/jmap", server.uri()),
    )))
}

async fn run(server: &MockServer, query: &str) -> async_graphql::Response {
    let schema = build_schema();
    schema.execute(request(query, client_for(server))).await
}

#[tokio::test]
async fn listing_without_bodies_makes_no_extra_fetch() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails { nodes { id subject } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let calls = calls(&server).await;
    let gets = calls.iter().filter(|c| c.method == "Email/get").count();
    assert_eq!(
        gets, 1,
        "header-only selection should not trigger the detail fetch"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string())),
        "list fetch must not ask for bodies"
    );
}

#[tokio::test]
async fn bodies_across_a_list_collapse_into_one_batched_fetch() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails { nodes { id textBody } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let calls = calls(&server).await;
    let detail: Vec<&Call> = calls
        .iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .collect();

    // The N+1 shape would be 5 detail calls of 1 id each.
    assert_eq!(detail.len(), 1, "expected a single batched detail fetch");
    assert_eq!(detail[0].ids.len(), 5, "batch should cover the whole page");
}

#[tokio::test]
async fn body_and_attachments_together_share_one_fetch() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails { nodes { textBody htmlBody attachments { name } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let detail = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .count();
    assert_eq!(
        detail, 1,
        "three body/attachment fields over three emails is still one fetch"
    );
}

// ============ Attachment laziness ============
//
// Metadata is free, each payload field does the least work that answers it, and
// text extraction — the expensive one — happens only when asked for.

#[tokio::test]
async fn attachment_metadata_downloads_nothing() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails { nodes { attachments { \
            blobId name contentType size disposition cid charset partId } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert!(
        downloads(&server).await.is_empty(),
        "attachment metadata comes with the email — it must not download blobs"
    );
}

#[tokio::test]
async fn base64_downloads_once_per_attachment() {
    let server = mock_server(3).await;
    let resp = run(&server, "{ emails { nodes { attachments { base64 } } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        downloads(&server).await.len(),
        3,
        "one download per attachment, no more"
    );
}

#[tokio::test]
async fn siblings_needing_the_same_blob_download_it_once() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 1) { nodes { attachments { base64 text image } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        downloads(&server).await.len(),
        1,
        "three payload fields on one attachment share a single download"
    );
}

#[tokio::test]
async fn text_extraction_is_not_triggered_by_other_fields() {
    // `text` is the only field that parses the document. Selecting everything
    // else — including the other payload fields — must not reach for it.
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails { nodes { attachments { name size base64 image } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let att = &data["emails"]["nodes"][0]["attachments"][0];
    assert!(att.get("text").is_none(), "text was not selected");
    assert!(
        att["base64"].is_string(),
        "base64 should resolve for any type"
    );
    assert!(
        att["image"].is_null(),
        "a text/plain attachment is not an image"
    );
}

#[tokio::test]
async fn text_extracts_document_content() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 1) { nodes { attachments { text } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let text = data["emails"]["nodes"][0]["attachments"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("attachment payload"),
        "expected the extracted body, got {text:?}"
    );
}

#[tokio::test]
async fn expensive_queries_are_costed_but_not_refused() {
    // Cost is guidance, not a gate. A wide fan-out of the most expensive field
    // still runs — the price is declared so a caller can pick a sensible page
    // size, not so the server can second-guess a legitimate request.
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ emails(first: 100) { nodes { attachments { text base64 image } } } }",
    )
    .await;
    assert!(
        resp.errors.is_empty(),
        "complexity must not refuse a query: {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn documented_examples_execute() {
    // The query shapes in the README and the MCP server instructions, run
    // against the real schema so a documented example cannot drift into being
    // invalid without a test noticing.
    let server = mock_server(10).await;
    let documented = [
        "{ emails(filter: { inMailbox: \"INBOX\" }, first: 10) { nodes { \
            subject from { name email } textBody \
            attachments { name contentType size cid text } } } }",
        "{ mailbox(name: \"INBOX\") { name children { name unreadEmails } \
            emails(first: 5) { nodes { subject \
              thread { total emails { subject textBody } } \
              mailboxes { name role } } } } }",
        "{ emails(filter: { unread: true }, sort: [{ property: SIZE, ascending: false }], \
            first: 20) { totalCount nodes { subject size } } }",
    ];

    for query in documented {
        let resp = run(&server, query).await;
        assert!(
            resp.errors.is_empty(),
            "documented example failed: {:?}\nquery: {query}",
            resp.errors
        );
    }
}

#[tokio::test]
async fn repeated_email_ids_are_deduplicated() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ a: email(id: \"e0\") { subject } b: email(id: \"e0\") { textBody } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get")
        .count();
    assert_eq!(gets, 1, "same id twice in one query is one fetch");
}

#[tokio::test]
async fn mailbox_nests_into_emails_and_back() {
    let server = mock_server(2).await;
    let resp = run(
        &server,
        "{ mailbox(name: \"INBOX\") { name emails(first: 2) { nodes { subject mailboxes { name role } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let mailbox = &data["mailbox"];
    assert_eq!(mailbox["name"], "Inbox");
    assert_eq!(mailbox["emails"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        mailbox["emails"]["nodes"][0]["mailboxes"][0]["role"],
        "inbox"
    );

    // The mailbox loader plus the client's own cache mean the whole query costs
    // one Mailbox/get, however many emails reference it.
    let mailbox_gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Mailbox/get")
        .count();
    assert_eq!(mailbox_gets, 1);
}

#[tokio::test]
async fn email_nests_into_its_thread() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ email(id: \"e0\") { subject thread { total emails { id textBody } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    assert_eq!(data["email"]["thread"]["total"], 3);
    assert_eq!(
        data["email"]["thread"]["emails"][0]["textBody"],
        "Body of e0"
    );

    let calls = calls(&server).await;
    assert_eq!(
        calls.iter().filter(|c| c.method == "Thread/get").count(),
        1,
        "one Thread/get for the conversation"
    );
    // e0 was already loaded for the outer selection, so the thread fetch only
    // needs the two it doesn't have.
    let thread_fetch = calls
        .iter()
        .rfind(|c| c.method == "Email/get")
        .expect("an Email/get");
    assert!(
        !thread_fetch.ids.contains(&"e0".to_string()),
        "already-cached email should not be refetched, got {:?}",
        thread_fetch.ids
    );
}

#[tokio::test]
async fn depth_limit_rejects_runaway_nesting() {
    let server = mock_server(1).await;
    // `parent` carries no fan-out cost, so this trips the depth guard rather
    // than the complexity guard.
    let deep = format!(
        "{{ mailbox(name: \"INBOX\") {{ {} id {} }} }}",
        "parent { ".repeat(16),
        "}".repeat(16)
    );
    let resp = run(&server, &deep).await;
    assert!(
        resp.errors.iter().any(|e| e.message.contains("too deep")),
        "expected a depth-limit error, got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn depth_limit_leaves_realistic_queries_alone() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        "{ mailbox(name: \"INBOX\") { emails(first: 1) { nodes { thread { emails { \
            attachments { name size } } } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
}

#[tokio::test]
async fn deep_cyclic_queries_are_still_refused() {
    // Depth remains capped even though complexity is not: the graph has cycles
    // (email -> thread -> emails -> ...) and nothing else bounds them.
    let server = mock_server(1).await;

    // Ten laps of the cycle is well past MAX_DEPTH without hard-coding a shape.
    let mut inner = "subject".to_string();
    for _ in 0..10 {
        inner = format!("thread {{ emails {{ {inner} }} }}");
    }
    let resp = run(&server, &format!("{{ emails {{ nodes {{ {inner} }} }} }}")).await;

    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("nested too deep")),
        "expected a depth-limit error, got {:?}",
        resp.errors
    );
}

#[test]
fn schema_exposes_the_nested_edges() {
    let sdl = build_schema().sdl();
    for edge in [
        "emails(",    // Mailbox.emails
        "children:",  // Mailbox.children
        "parent:",    // Mailbox.parent
        "thread:",    // Email.thread
        "mailboxes:", // Email.mailboxes
    ] {
        assert!(sdl.contains(edge), "SDL missing `{edge}`:\n{sdl}");
    }
    assert!(
        !sdl.contains("EmailSummary"),
        "EmailSummary should be folded into Email"
    );
}

// ============ Connections, filters, pagination ============

/// The JMAP `filter` argument sent for the first `Email/query` in a request.
async fn sent_filter(server: &MockServer) -> Value {
    for req in server.received_requests().await.unwrap_or_default() {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for mc in body["methodCalls"].as_array().into_iter().flatten() {
            if mc.get(0).and_then(Value::as_str) == Some("Email/query") {
                return mc[1]["filter"].clone();
            }
        }
    }
    Value::Null
}

/// Every `Email/query` argument object sent, in order.
async fn query_args(server: &MockServer) -> Vec<Value> {
    let mut out = Vec::new();
    for req in server.received_requests().await.unwrap_or_default() {
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for mc in body["methodCalls"].as_array().into_iter().flatten() {
            if mc.get(0).and_then(Value::as_str) == Some("Email/query") {
                out.push(mc[1].clone());
            }
        }
    }
    out
}

#[tokio::test]
async fn or_filter_reaches_jmap_as_a_filter_operator() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { unread: true,
                              or: [{ from: "a@b.com" }, { from: "c@d.com" }] })
              { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "operator": "AND",
            "conditions": [
                { "notKeyword": "$seen" },
                { "operator": "OR", "conditions": [
                    { "from": "a@b.com" }, { "from": "c@d.com" }] }
            ]
        }),
        "the filter tree must survive into JMAP, not be flattened"
    );
}

#[tokio::test]
async fn mailbox_names_in_filters_resolve_to_ids() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailbox: "INBOX", subject: "x" }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(sent_filter(&server).await["inMailbox"], "mb1");
}

#[tokio::test]
async fn unknown_mailbox_in_a_filter_is_an_error() {
    let server = mock_server(1).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailbox: "Nope" }) { nodes { id } } }"#,
    )
    .await;
    assert!(
        resp.errors.iter().any(|e| e.message.contains("Nope")),
        "expected an unknown-mailbox error, got {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn mailbox_emails_are_scoped_to_that_mailbox() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ mailbox(name: "INBOX") { emails(filter: { unread: true }) { nodes { id } } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "operator": "AND",
            "conditions": [{ "notKeyword": "$seen" }, { "inMailbox": "mb1" }]
        }),
        "the caller's filter must be ANDed with the mailbox constraint"
    );
}

#[tokio::test]
async fn sort_is_passed_through() {
    let server = mock_server(2).await;
    let resp = run(
        &server,
        "{ emails(sort: [{ property: SUBJECT, ascending: true }]) { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["sort"],
        json!([{ "property": "subject", "isAscending": true }])
    );
}

#[tokio::test]
async fn cursors_are_ids_and_paginate_forward() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3) { edges { cursor node { id } } pageInfo { hasNextPage endCursor } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 3);
    assert_eq!(
        edges[0]["cursor"], "e0",
        "cursors are email IDs, not positions"
    );
    assert_eq!(edges[0]["node"]["id"], "e0");
    assert_eq!(edges[2]["cursor"], "e2");
    assert_eq!(data["emails"]["pageInfo"]["hasNextPage"], true);

    // Resume from the cursor and confirm we get the next window, not a repeat.
    let server2 = mock_server(10).await;
    let resp = run(
        &server2,
        "{ emails(first: 3, after: \"e2\") { edges { cursor node { id } } \
            pageInfo { hasPreviousPage } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges[0]["cursor"], "e3");
    assert_eq!(edges[0]["node"]["id"], "e3");
    assert_eq!(data["emails"]["pageInfo"]["hasPreviousPage"], true);
}

#[tokio::test]
async fn last_paginates_backward_from_the_end() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(last: 2) { edges { cursor node { id } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let edges = data["emails"]["edges"].as_array().unwrap();
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0]["node"]["id"], "e8");
    assert_eq!(edges[1]["node"]["id"], "e9");
    assert_eq!(edges[1]["cursor"], "e9");
}

#[tokio::test]
async fn total_count_is_only_computed_when_selected() {
    let server = mock_server(7).await;
    let resp = run(&server, "{ emails(first: 2) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["calculateTotal"],
        json!(false),
        "a query that doesn't select totalCount must not pay for it"
    );

    let server = mock_server(7).await;
    let resp = run(&server, "{ emails(first: 2) { totalCount nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["calculateTotal"], json!(true));
    assert_eq!(resp.data.into_json().unwrap()["emails"]["totalCount"], 7);
}

#[tokio::test]
async fn counting_alone_fetches_no_emails() {
    let server = mock_server(7).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { unread: true }) { totalCount } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(resp.data.into_json().unwrap()["emails"]["totalCount"], 7);

    let gets = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get")
        .count();
    assert_eq!(gets, 0, "asking only for a count must not fetch any email");
}

#[tokio::test]
async fn page_size_is_capped() {
    let server = mock_server(3).await;
    let resp = run(&server, "{ emails(first: 5000) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["limit"], json!(100));
}

#[tokio::test]
async fn collapse_threads_reaches_jmap() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails(collapseThreads: true) { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(query_args(&server).await[0]["collapseThreads"], json!(true));
}

#[tokio::test]
async fn results_keep_the_sort_order_jmap_returned() {
    let server = mock_server(5).await;
    let resp = run(&server, "{ emails(first: 5) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    // `Email/get` makes no ordering promise, so this asserts we re-apply the
    // order from `Email/query` rather than trusting the response.
    assert_eq!(ids, ["e0", "e1", "e2", "e3", "e4"]);
}

#[tokio::test]
async fn deprecated_search_emails_still_works() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ searchEmails(query: "invoice", unread: true) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        sent_filter(&server).await,
        json!({ "text": "invoice", "notKeyword": "$seen" })
    );
}

#[tokio::test]
async fn bodies_batch_across_a_connection_page() {
    let server = mock_server(8).await;
    let resp = run(&server, "{ emails(first: 8) { nodes { id textBody } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let detail: Vec<Call> = calls(&server)
        .await
        .into_iter()
        .filter(|c| c.method == "Email/get" && c.properties.contains(&"textBody".to_string()))
        .collect();
    assert_eq!(detail.len(), 1, "one batched detail fetch for the page");
    assert_eq!(detail[0].ids.len(), 8);
}

// ============ Anchor cursors and the wider JMAP arg surface ============

#[tokio::test]
async fn cursors_use_jmap_anchors_not_offsets() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3, after: \"e2\") { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(args["anchor"], "e2");
    assert_eq!(args["anchorOffset"], 1);
    assert!(
        args.get("position").is_none(),
        "an anchored query must not also send a position"
    );
}

#[tokio::test]
async fn pagination_is_stable_when_new_mail_arrives() {
    // Page 1 of a 10-message inbox.
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 3) { edges { cursor node { id } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    let page1 = resp.data.into_json().unwrap();
    let last_cursor = page1["emails"]["edges"][2]["cursor"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(last_cursor, "e2");

    // Two new messages land at the head of the result set before page 2. With a
    // positional cursor, `after: "2"` would now return e0..e2 again — the two
    // newcomers having pushed everything down. The anchor still names e2.
    let server = MockServer::start().await;
    let ids: Vec<String> = (0..12).map(|i| format!("e{i}")).collect();
    let shifted = ids.clone();
    Mock::given(method("POST"))
        .and(path("/jmap"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let mut responses = Vec::new();
            for mc in body["methodCalls"].as_array().into_iter().flatten() {
                let name = mc[0].as_str().unwrap_or("");
                let args = &mc[1];
                let tag = mc[2].as_str().unwrap_or("c0");
                // "new0"/"new1" prepended: every old message shifted by 2.
                let all: Vec<String> = ["new0".to_string(), "new1".to_string()]
                    .into_iter()
                    .chain(shifted.iter().take(10).cloned())
                    .collect();
                let payload = match name {
                    "Email/query" => {
                        let anchor = args["anchor"].as_str().unwrap();
                        let idx = all.iter().position(|i| i == anchor).unwrap() as i64;
                        let off = args["anchorOffset"].as_i64().unwrap_or(0);
                        let start = (idx + off).max(0) as usize;
                        let limit = args["limit"].as_u64().unwrap_or(10) as usize;
                        json!({
                            "ids": all.iter().skip(start).take(limit).collect::<Vec<_>>(),
                            "position": start
                        })
                    }
                    "Email/get" => json!({
                        "list": all.iter().skip(5).take(3)
                            .map(|id| email_json(id, "t1", false)).collect::<Vec<_>>(),
                        "notFound": []
                    }),
                    _ => json!({ "list": [], "notFound": [] }),
                };
                responses.push(json!([name, payload, tag]));
            }
            ResponseTemplate::new(200).set_body_json(json!({ "methodResponses": responses }))
        })
        .mount(&server)
        .await;

    let resp = run(
        &server,
        &format!("{{ emails(first: 3, after: \"{last_cursor}\") {{ nodes {{ id }} }} }}"),
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["e3", "e4", "e5"],
        "the page after e2 must still start at e3, not repeat e0..e2"
    );
}

#[tokio::test]
async fn last_without_before_uses_a_negative_position() {
    let server = mock_server(10).await;
    let resp = run(&server, "{ emails(last: 2) { nodes { id } } }").await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(
        args["position"], -2,
        "JMAP counts a negative position from the end"
    );
    assert_eq!(
        args["calculateTotal"], false,
        "counting from the end needs no total"
    );
    assert_eq!(query_args(&server).await.len(), 1, "and only one call");
}

#[tokio::test]
async fn before_anchors_backward_from_the_cursor() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(last: 3, before: \"e7\") { nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let args = &query_args(&server).await[0];
    assert_eq!(args["anchor"], "e7");
    assert_eq!(args["anchorOffset"], -3);

    let data = resp.data.into_json().unwrap();
    let ids: Vec<&str> = data["emails"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e4", "e5", "e6"], "the three ending just before e7");
}

#[tokio::test]
async fn a_stale_cursor_says_how_to_recover() {
    let server = mock_server(5).await;
    let resp = run(
        &server,
        "{ emails(first: 2, after: \"deleted\") { nodes { id } } }",
    )
    .await;
    let msg = resp
        .errors
        .first()
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("no longer in this result set") && msg.contains("Restart pagination"),
        "expected actionable stale-anchor advice, got {msg:?}"
    );
}

#[tokio::test]
async fn conflicting_pagination_args_are_rejected() {
    let server = mock_server(3).await;
    for (q, want) in [
        ("{ emails(first: 2, last: 2) { nodes { id } } }", "not both"),
        (
            "{ emails(after: \"e0\", before: \"e2\") { nodes { id } } }",
            "not both",
        ),
    ] {
        let resp = run(&server, q).await;
        assert!(
            resp.errors.iter().any(|e| e.message.contains(want)),
            "{q} should be rejected, got {:?}",
            resp.errors
        );
    }
}

#[tokio::test]
async fn query_state_and_position_are_exposed() {
    let server = mock_server(10).await;
    let resp = run(
        &server,
        "{ emails(first: 2, after: \"e4\") { position queryState nodes { id } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    assert_eq!(data["emails"]["position"], 5);
    assert_eq!(data["emails"]["queryState"], "qs-1");
}

#[tokio::test]
async fn thread_keyword_and_header_conditions_reach_jmap() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: {
                someInThreadHaveKeyword: "$flagged"
                noneInThreadHaveKeyword: "$junk"
                header: ["List-Id", "rust-lang"]
            }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    assert_eq!(
        sent_filter(&server).await,
        json!({
            "someInThreadHaveKeyword": "$flagged",
            "noneInThreadHaveKeyword": "$junk",
            "header": ["List-Id", "rust-lang"]
        })
    );
}

#[tokio::test]
async fn in_mailbox_other_than_resolves_names() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(filter: { inMailboxOtherThan: ["INBOX"] }) { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        sent_filter(&server).await,
        json!({ "inMailboxOtherThan": ["mb1"] })
    );
}

#[tokio::test]
async fn keyword_sort_carries_its_keyword() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        r#"{ emails(sort: [{ property: SOME_IN_THREAD_HAVE_KEYWORD, keyword: "$flagged" }])
              { nodes { id } } }"#,
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
    assert_eq!(
        query_args(&server).await[0]["sort"],
        json!([{ "property": "someInThreadHaveKeyword", "isAscending": false,
                 "keyword": "$flagged" }])
    );
}

#[tokio::test]
async fn keyword_sort_without_keyword_is_rejected_before_any_call() {
    let server = mock_server(3).await;
    let resp = run(
        &server,
        "{ emails(sort: [{ property: HAS_KEYWORD }]) { nodes { id } } }",
    )
    .await;
    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("requires a `keyword`")),
        "got {:?}",
        resp.errors
    );
    assert!(
        query_args(&server).await.is_empty(),
        "an invalid comparator must not reach the server"
    );
}
