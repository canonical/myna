//! `myna-text-v1`: JSON lines over a Unix socket to a text model provider.
//!
//! One JSON object per line each way. A call names an `op`; the reply is
//! one object, with `error` set when the call failed. The daemon chooses
//! the model (`model` is an id from the provider's own capabilities); the
//! provider only runs it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub type Params = BTreeMap<String, Value>;

/// What the daemon asks of a provider.
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Call<'a> {
    Capabilities,
    /// Load `model` for `task` so the first `run` is not the one to pay.
    Warm {
        model: &'a str,
        task: &'a str,
        params: &'a Params,
    },
    Run {
        model: &'a str,
        task: &'a str,
        params: &'a Params,
        text: &'a str,
        deadline_ms: u64,
    },
}

/// What a provider says it can do.
#[derive(Debug, Deserialize)]
pub struct Capabilities {
    pub protocol: String,
    /// The contract number the provider speaks; absent means 1.
    #[serde(default)]
    pub contract: Option<u32>,
    #[serde(default)]
    pub models: Vec<Capability>,
}

/// One model of a provider, and the requests it covers.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Capability {
    pub id: String,
    pub task: String,
    /// Parameters the model is limited to: `language: [en]`.
    #[serde(default)]
    pub covers: BTreeMap<String, Vec<String>>,
    /// Free-form parameters the model takes whatever they say, such as an
    /// `instruction`. A parameter in neither list is not covered.
    #[serde(default)]
    pub accepts: Vec<String>,
    /// Typical time for one sentence, as the provider measured it.
    pub latency_ms: Option<u64>,
    /// 0..1, the provider's own estimate; only compared across its models
    /// and, failing that, across providers.
    pub quality: Option<f64>,
    /// What kind of step it is (`edit`, `translate`, `rewrite`); the client
    /// derives the output check from it. Absent: the task's default class.
    pub class: Option<String>,
    /// `sentence` or `dictation`: how much text one call wants.
    pub needs: Option<String>,
    /// What a picker needs to offer the step's parameters.
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    pub license: Option<String>,
    pub attribution: Option<String>,
    pub download_bytes: Option<u64>,
    /// Where the model came from.
    pub source: Option<String>,
}

/// One parameter of a step, as a provider describes it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    /// `language` or `text`.
    pub kind: String,
    /// `source`, `target` or `free`.
    pub role: String,
    #[serde(default)]
    pub required: bool,
}

impl Capability {
    /// Whether the model can do `task` with `params`.
    pub fn covers(&self, task: &str, params: &Params) -> bool {
        self.task == task
            && params
                .iter()
                .all(|(key, value)| match self.covers.get(key) {
                    Some(allowed) => value
                        .as_str()
                        .is_some_and(|v| allowed.iter().any(|a| a == v)),
                    None => self.accepts.contains(key),
                })
    }
}

#[derive(Deserialize)]
struct Reply {
    text: Option<String>,
    error: Option<String>,
}

/// Why a call got no answer. `Refused` carries the provider's words, which
/// may quote the dictation, so callers keep them out of logs.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider refused: {0}")]
    Refused(String),
    /// It was still working when the deadline passed: slow, not gone.
    #[error("provider timed out")]
    TimedOut,
}

async fn exchange(path: &Path, call: &Call<'_>, within: Duration) -> Result<String, WireError> {
    let unavailable = |e: std::io::Error| WireError::Unavailable(e.to_string());
    let mut line =
        serde_json::to_string(call).map_err(|e| WireError::Unavailable(e.to_string()))?;
    line.push('\n');
    let io = async {
        let stream = UnixStream::connect(path).await?;
        let (read, mut write) = stream.into_split();
        write.write_all(line.as_bytes()).await?;
        let mut reply = String::new();
        BufReader::new(read).read_line(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    };
    match tokio::time::timeout(within, io).await {
        Ok(reply) => reply.map_err(unavailable),
        Err(_) => Err(WireError::TimedOut),
    }
}

fn reply_of(line: &str) -> Result<Reply, WireError> {
    let reply: Reply =
        serde_json::from_str(line).map_err(|_| WireError::Refused("unreadable reply".into()))?;
    match reply.error {
        Some(error) => Err(WireError::Refused(error)),
        None => Ok(reply),
    }
}

pub async fn capabilities(path: &Path, within: Duration) -> Result<Capabilities, WireError> {
    let line = exchange(path, &Call::Capabilities, within).await?;
    let caps: Capabilities = serde_json::from_str(&line)
        .map_err(|_| WireError::Refused("unreadable capabilities".into()))?;
    if caps.protocol == "myna-text-v1" {
        Ok(caps)
    } else {
        Err(WireError::Refused("another protocol".into()))
    }
}

pub async fn warm(
    path: &Path,
    model: &str,
    task: &str,
    params: &Params,
    within: Duration,
) -> Result<(), WireError> {
    let call = Call::Warm {
        model,
        task,
        params,
    };
    reply_of(&exchange(path, &call, within).await?).map(|_| ())
}

pub async fn run(
    path: &Path,
    model: &str,
    task: &str,
    params: &Params,
    text: &str,
    within: Duration,
) -> Result<String, WireError> {
    let call = Call::Run {
        model,
        task,
        params,
        text,
        deadline_ms: u64::try_from(within.as_millis()).unwrap_or(u64::MAX),
    };
    reply_of(&exchange(path, &call, within).await?)?
        .text
        .ok_or_else(|| WireError::Refused("no text".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::UnixListener;

    fn params(value: Value) -> Params {
        serde_json::from_value(value).unwrap()
    }

    fn capability(json: Value) -> Capability {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn calls_name_their_op_and_carry_every_field() {
        let p = params(json!({"language": "en", "to": "fr"}));
        let sent = |call: Call| serde_json::to_value(call).unwrap();
        assert_eq!(sent(Call::Capabilities), json!({"op": "capabilities"}));
        assert_eq!(
            sent(Call::Warm {
                model: "m",
                task: "text.translate",
                params: &p
            }),
            json!({"op": "warm", "model": "m", "task": "text.translate",
                   "params": {"language": "en", "to": "fr"}})
        );
        assert_eq!(
            sent(Call::Run {
                model: "m",
                task: "text.translate",
                params: &p,
                text: "Hi.",
                deadline_ms: 250
            }),
            json!({"op": "run", "model": "m", "task": "text.translate",
                   "params": {"language": "en", "to": "fr"}, "text": "Hi.",
                   "deadline_ms": 250})
        );
    }

    #[test]
    fn a_model_covers_only_the_requests_it_lists() {
        let tagger = capability(json!({
            "id": "fdt", "task": "text.cleanup", "covers": {"language": ["en"]}
        }));
        let none = params(json!({}));
        assert!(tagger.covers("text.cleanup", &none));
        assert!(tagger.covers("text.cleanup", &params(json!({"language": "en"}))));
        assert!(!tagger.covers("text.cleanup", &params(json!({"language": "fr"}))));
        assert!(!tagger.covers("text.translate", &none));
        assert!(!tagger.covers("text.cleanup", &params(json!({"language": 7}))));
        // A parameter it never mentions is one it cannot honour.
        let asks = params(json!({"dialect": "be brief"}));
        assert!(!tagger.covers("text.cleanup", &asks));
        let llm = capability(json!({
            "id": "m", "task": "text.cleanup", "accepts": ["dialect"]
        }));
        assert!(llm.covers("text.cleanup", &asks));
        assert!(!llm.covers("text.cleanup", &params(json!({"style": "x"}))));
    }

    #[test]
    fn a_reply_with_an_error_is_a_refusal_whatever_else_it_says() {
        assert!(matches!(
            reply_of(r#"{"text": "x", "error": "no"}"#),
            Err(WireError::Refused(e)) if e == "no"
        ));
        assert!(matches!(reply_of("nonsense"), Err(WireError::Refused(_))));
        assert_eq!(
            reply_of(r#"{"text":"x"}"#).unwrap().text.as_deref(),
            Some("x")
        );
    }

    /// Answers one line per connection from `replies`, returning what it read.
    fn serve(path: &Path, replies: Vec<&'static str>) -> tokio::task::JoinHandle<Vec<Value>> {
        let listener = UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                seen.push(serde_json::from_str(&line).unwrap());
                write.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        })
    }

    #[test]
    fn contract_two_fields_are_read_and_all_are_optional() {
        let old = capability(json!({"id": "m", "task": "text.cleanup"}));
        assert_eq!(
            old,
            Capability {
                id: "m".into(),
                task: "text.cleanup".into(),
                ..Capability::default()
            }
        );
        let new = capability(json!({
            "id": "m", "task": "text.translate", "class": "translate",
            "needs": "dictation", "license": "MPL-2.0", "attribution": "Mozilla",
            "download_bytes": 33353728, "source": "https://example.org/m",
            "params": [{"name": "to", "kind": "language", "role": "target",
                        "required": true},
                       {"name": "dialect", "kind": "text", "role": "free"}],
            "future": 1
        }));
        assert_eq!(new.class.as_deref(), Some("translate"));
        assert_eq!(new.needs.as_deref(), Some("dictation"));
        assert_eq!(new.license.as_deref(), Some("MPL-2.0"));
        assert_eq!(new.attribution.as_deref(), Some("Mozilla"));
        assert_eq!(new.download_bytes, Some(33_353_728));
        assert_eq!(new.source.as_deref(), Some("https://example.org/m"));
        assert_eq!(new.params.len(), 2);
        assert!(new.params[0].required && !new.params[1].required);
        assert_eq!(new.params[0].role, "target");
        let caps: Capabilities =
            serde_json::from_value(json!({"protocol": "myna-text-v1", "contract": 2})).unwrap();
        assert_eq!(caps.contract, Some(2));
        let caps: Capabilities =
            serde_json::from_value(json!({"protocol": "myna-text-v1"})).unwrap();
        assert_eq!(caps.contract, None);
    }

    #[tokio::test]
    async fn the_three_calls_round_trip_against_a_provider() {
        let dir = std::env::temp_dir().join(format!("myna-wire-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let server = serve(
            &path,
            vec![
                r#"{"protocol":"myna-text-v1","models":[{"id":"fdt","task":"text.cleanup","latency_ms":5}]}
"#,
                "{\"ok\":true}\n",
                "{\"text\":\"Hi.\"}\n",
                "{\"error\":\"no such model\"}\n",
                "{\"protocol\":\"other\"}\n",
            ],
        );
        let secs = Duration::from_secs(5);
        let caps = capabilities(&path, secs).await.unwrap();
        assert_eq!(caps.models[0].id, "fdt");
        assert_eq!(caps.models[0].latency_ms, Some(5));
        let p = params(json!({"language": "en"}));
        warm(&path, "fdt", "text.cleanup", &p, secs).await.unwrap();
        let text = run(&path, "fdt", "text.cleanup", &p, "um hi", secs).await;
        assert_eq!(text.unwrap(), "Hi.");
        assert!(matches!(
            run(&path, "x", "text.cleanup", &p, "um hi", secs).await,
            Err(WireError::Refused(e)) if e == "no such model"
        ));
        assert!(matches!(
            capabilities(&path, secs).await,
            Err(WireError::Refused(_))
        ));
        let seen = server.await.unwrap();
        assert_eq!(seen[2]["text"], "um hi");
        assert_eq!(seen[2]["deadline_ms"], 5000);
        assert_eq!(seen[2]["params"], json!({"language": "en"}));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_missing_or_silent_provider_is_unavailable() {
        let secs = Duration::from_secs(1);
        assert!(matches!(
            capabilities(Path::new("/nonexistent/myna-text"), secs).await,
            Err(WireError::Unavailable(_))
        ));
        let dir = std::env::temp_dir().join(format!("myna-wire-silent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let _silent = UnixListener::bind(&path).unwrap();
        let started = std::time::Instant::now();
        let hung = capabilities(&path, Duration::from_millis(50)).await;
        assert!(matches!(hung, Err(WireError::TimedOut)));
        assert!(started.elapsed() < secs);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
