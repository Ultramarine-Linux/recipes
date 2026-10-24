//! GitHub webhook verification and routing. Pure, so it's easy to test.

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

/// Check GitHub's `X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the raw
/// body>`, in constant time.
#[must_use]
pub fn verify_signature(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(signature) = header
        .and_then(|header| header.strip_prefix("sha256="))
        .and_then(|hex| hex::decode(hex).ok())
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// Whether `value` is a full lowercase commit sha.
#[must_use]
pub fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What to do with a verified delivery.
#[derive(Debug, PartialEq, Eq)]
pub struct Route {
    /// HTTP status to answer with.
    pub status: u16,
    pub message: String,
    /// The commit to build, for pushes to the watched branch.
    pub build: Option<String>,
}

impl Route {
    fn reply(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            build: None,
        }
    }
}

/// `ping` → 200; a push to `refs/heads/<branch>` of `repo` → 202 and build
/// its `after` commit; anything else → 204.
#[must_use]
pub fn route_event(event: &str, payload: &Value, repo: &str, branch: &str) -> Route {
    match event {
        "ping" => return Route::reply(200, "pong"),
        "push" => {}
        other => return Route::reply(204, format!("ignored {other} event")),
    }
    let field = |pointer| payload.pointer(pointer).and_then(Value::as_str);
    if !field("/repository/full_name").is_some_and(|name| name.eq_ignore_ascii_case(repo)) {
        return Route::reply(204, "ignored push to another repository");
    }
    let reference = field("/ref").unwrap_or_default();
    if reference != format!("refs/heads/{branch}") {
        return Route::reply(204, format!("ignored push to {reference}"));
    }
    let deleted = payload.get("deleted").and_then(Value::as_bool) == Some(true);
    let Some(after) = field("/after").filter(|after| !deleted && is_sha(after)) else {
        return Route::reply(204, "ignored branch deletion");
    };
    Route {
        status: 202,
        message: format!("queued {after}"),
        build: Some(after.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verifies_signatures() {
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let header = sign(b"secret", body);
        assert!(verify_signature(b"secret", body, Some(&header)));
        assert!(!verify_signature(b"other", body, Some(&header)));
        assert!(!verify_signature(b"secret", b"{}", Some(&header)));
        assert!(!verify_signature(b"secret", body, None));
        assert!(!verify_signature(b"secret", body, Some(&header[7..])));
        assert!(!verify_signature(b"secret", body, Some("sha256=zz")));
        assert!(!verify_signature(b"secret", body, Some(&header[..20])));
        // Uppercase hex is still the same signature.
        assert!(verify_signature(
            b"secret",
            body,
            Some(&format!("sha256={}", header[7..].to_uppercase()))
        ));
    }

    #[test]
    fn matches_githubs_documented_example() {
        // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
        assert!(verify_signature(
            b"It's a Secret to Everybody",
            b"Hello, World!",
            Some("sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17")
        ));
    }

    #[test]
    fn routes_events() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let push = |reference: &str, after: &str| {
            json!({ "ref": reference, "after": after, "deleted": false,
                    "repository": { "full_name": "ultramarine-server/Recipes" } })
        };
        let route = |event, payload: &Value| {
            route_event(event, payload, "Ultramarine-Server/recipes", "main")
        };

        assert_eq!(route("ping", &json!({})).status, 200);
        assert_eq!(route("issues", &json!({})).status, 204);
        let queued = route("push", &push("refs/heads/main", sha));
        assert_eq!((queued.status, queued.build.as_deref()), (202, Some(sha)));
        assert_eq!(route("push", &push("refs/heads/dev", sha)).status, 204);
        assert_eq!(route("push", &push("refs/tags/main", sha)).status, 204);
        assert_eq!(route("push", &push("refs/heads/main", "HEAD")).status, 204);
        let mut deleted = push("refs/heads/main", sha);
        deleted["deleted"] = json!(true);
        assert_eq!(route("push", &deleted).build, None);
        let mut other = push("refs/heads/main", sha);
        other["repository"]["full_name"] = json!("someone/recipes");
        assert_eq!(route("push", &other).status, 204);
    }
}
