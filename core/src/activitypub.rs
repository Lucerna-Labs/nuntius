use serde::{Deserialize, Serialize};

// ============================================================================
// WebFinger Protocol (RFC 7033)
// Required for discovery on the Fediverse (e.g., searching @station@ccc.com)
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WebFingerLink {
    pub rel: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    pub href: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WebFingerResponse {
    pub subject: String,
    pub aliases: Vec<String>,
    pub links: Vec<WebFingerLink>,
}

// ============================================================================
// ActivityPub / ActivityStreams 2.0 (W3C Recommendation)
// The core data models for federated profiles and communication.
// ============================================================================

/// Represents a user or station profile on the Fediverse.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Actor {
    #[serde(rename = "@context")]
    pub context: Vec<String>,
    pub id: String,
    #[serde(rename = "type")]
    pub actor_type: String, // e.g., "Service", "Person", "Application"
    #[serde(rename = "preferredUsername")]
    pub preferred_username: String,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub inbox: String,
    pub outbox: String,
    pub followers: String,
    pub following: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Image>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(rename = "publicKey")]
    pub public_key: PublicKey,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Image {
    #[serde(rename = "type")]
    pub image_type: String, // e.g., "Image"
    #[serde(rename = "mediaType")]
    pub media_type: String, // e.g., "image/jpeg"
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PublicKey {
    pub id: String,
    pub owner: String,
    #[serde(rename = "publicKeyPem")]
    pub public_key_pem: String,
}

/// A generic wrapper for ActivityPub Activities (Create, Follow, Undo, etc.)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Activity {
    #[serde(rename = "@context")]
    pub context: String,
    pub id: String,
    #[serde(rename = "type")]
    pub activity_type: String,
    pub actor: String,
    // The object could be a String (URL) or an embedded JSON Object (Note, Article, etc.)
    // For simplicity in the core stub, we accept dynamic JSON values.
    pub object: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<Vec<String>>,
}

// ============================================================================
// Core ActivityPub Axum Handlers (Stubs)
// ============================================================================
use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use std::collections::HashMap;

/// Handles WebFinger queries (e.g., `/.well-known/webfinger?resource=acct:station@domain.com`)
pub async fn webfinger_handler(Query(params): Query<HashMap<String, String>>) -> impl IntoResponse {
    let resource = match params.get("resource") {
        Some(r) => r,
        None => return (StatusCode::BAD_REQUEST, "Missing resource parameter").into_response(),
    };

    // A real implementation would query the database to ensure the resource (acct) exists.
    // Here we provide the static stub format pointing to our Actor profile.
    let response = WebFingerResponse {
        subject: resource.to_string(),
        aliases: vec!["https://ccc.example.com/users/station".to_string()],
        links: vec![WebFingerLink {
            rel: "self".to_string(),
            link_type: Some("application/activity+json".to_string()),
            href: "https://ccc.example.com/users/station".to_string(),
        }],
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// The public profile endpoint requested by Fediverse instances.
pub async fn actor_profile_handler() -> impl IntoResponse {
    // A real implementation would pull from configuration or CccDB.
    let actor = Actor {
        context: vec![
            "https://www.w3.org/ns/activitystreams".to_string(),
            "https://w3id.org/security/v1".to_string(),
        ],
        id: "https://ccc.example.com/users/station".to_string(),
        actor_type: "Service".to_string(), // Representing the CCC Radio/Video Station
        preferred_username: "station".to_string(),
        name: Some("Creator Command Center".to_string()),
        summary: Some("The official broadcast feed.".to_string()),
        inbox: "https://ccc.example.com/users/station/inbox".to_string(),
        outbox: "https://ccc.example.com/users/station/outbox".to_string(),
        followers: "https://ccc.example.com/users/station/followers".to_string(),
        following: "https://ccc.example.com/users/station/following".to_string(),
        icon: None,
        image: None,
        public_key: PublicKey {
            id: "https://ccc.example.com/users/station#main-key".to_string(),
            owner: "https://ccc.example.com/users/station".to_string(),
            public_key_pem: "-----BEGIN PUBLIC KEY-----\n...\n-----END PUBLIC KEY-----".to_string(),
        },
    };

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/activity+json; charset=utf-8",
        )],
        Json(actor),
    )
        .into_response()
}

/// The Inbox receives activities (like Follow requests) from other federated servers.
pub async fn inbox_handler() -> impl IntoResponse {
    // E.g., validating HTTP Signatures and processing incoming `Follow` activities
    // to add the remote user to our `CccDB` followers list.
    StatusCode::ACCEPTED
}

/// The Outbox serves public posts (like new Podcasts or Blogs) to clients.
pub async fn outbox_handler() -> impl IntoResponse {
    // E.g., querying `CccDB` for the latest posts and returning them as `OrderedCollection`.
    StatusCode::OK
}
