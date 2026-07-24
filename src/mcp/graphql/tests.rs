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
        "hasAttachment": false,
    });
    if with_body {
        e["textBody"] = json!([{ "partId": "1", "type": "text/plain" }]);
        e["bodyValues"] = json!({ "1": { "value": format!("Body of {id}") } });
        e["attachments"] = json!([]);
    }
    e
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
                "Email/query" => json!({ "ids": ids }),
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
    let resp = run(&server, "{ emails(mailbox: \"INBOX\") { id subject } }").await;
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
    let resp = run(&server, "{ emails(mailbox: \"INBOX\") { id textBody } }").await;
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
        "{ emails(mailbox: \"INBOX\") { textBody htmlBody attachments { name } } }",
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
        "{ mailbox(name: \"INBOX\") { name emails(limit: 2) { subject mailboxes { name role } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);

    let data = resp.data.into_json().unwrap();
    let mailbox = &data["mailbox"];
    assert_eq!(mailbox["name"], "Inbox");
    assert_eq!(mailbox["emails"].as_array().unwrap().len(), 2);
    assert_eq!(mailbox["emails"][0]["mailboxes"][0]["role"], "inbox");

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
        "{ mailbox(name: \"INBOX\") { emails(limit: 1) { thread { emails { \
            attachments { name content { size } } } } } } }",
    )
    .await;
    assert!(resp.errors.is_empty(), "{:?}", resp.errors);
}

#[tokio::test]
async fn complexity_limit_rejects_excessive_fan_out() {
    let server = mock_server(1).await;
    // 100 emails × their threads × those threads' emails and attachments.
    let query = "{ emails(mailbox: \"INBOX\", limit: 100) { \
                    thread { emails { attachments { name content { size } } } } } }";
    let resp = run(&server, query).await;
    assert!(
        resp.errors
            .iter()
            .any(|e| e.message.contains("too complex")),
        "expected a complexity-limit error, got {:?}",
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
