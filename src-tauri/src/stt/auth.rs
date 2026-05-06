//! Auth schemes for STT providers. Each cloud STT API authenticates
//! differently; this enum encodes the shape so adapters don't duplicate the
//! header/key plumbing per provider.

use reqwest::RequestBuilder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Authorization-style header. `prefix` is the literal prepended to the
    /// key, e.g. `Some("Bearer ")` for OpenAI, `Some("Token ")` for Deepgram,
    /// `None` for raw-key headers like `xi-api-key`.
    Header { name: &'static str, prefix: Option<&'static str> },
    /// Key passed as `?<name>=<key>` in the URL.
    QueryParam { name: &'static str },
    /// Key passed as a JSON body field. The adapter's `transcribe`
    /// implementation must merge it into the JSON payload manually since
    /// RequestBuilder can't rewrite the body in flight.
    BodyField { name: &'static str },
}

impl Auth {
    pub fn apply(&self, req: RequestBuilder, key: &str) -> RequestBuilder {
        match self {
            Auth::Header { name, prefix } => {
                let value = match prefix {
                    Some(p) => format!("{p}{key}"),
                    None => key.to_string(),
                };
                req.header(*name, value)
            }
            Auth::QueryParam { name } => req.query(&[(*name, key)]),
            Auth::BodyField { .. } => req,
        }
    }

    pub fn body_field_name(&self) -> Option<&'static str> {
        match self {
            Auth::BodyField { name } => Some(name),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_prefix_builds_bearer() {
        let auth = Auth::Header { name: "Authorization", prefix: Some("Bearer ") };
        let key = "sk-test123";
        match auth {
            Auth::Header { prefix: Some(p), .. } => {
                assert_eq!(format!("{p}{key}"), "Bearer sk-test123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn header_without_prefix_uses_raw_key() {
        let auth = Auth::Header { name: "xi-api-key", prefix: None };
        match auth {
            Auth::Header { prefix: None, name } => assert_eq!(name, "xi-api-key"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn body_field_name_only_returns_for_body_variant() {
        assert_eq!(Auth::BodyField { name: "api_key" }.body_field_name(), Some("api_key"));
        assert_eq!(Auth::Header { name: "Authorization", prefix: None }.body_field_name(), None);
        assert_eq!(Auth::QueryParam { name: "key" }.body_field_name(), None);
    }
}
