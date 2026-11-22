//! A text model behind an OpenAI-compatible API served on a Unix socket.
//!
//! Only what the daemon needs: `GET /v1/models` and
//! `POST /v1/chat/completions`, as HTTP/1.1 written by hand over a
//! [`UnixStream`], so no HTTP or TLS crate enters the confined client.
//! A provider that only listens on TCP is out of reach until it serves a
//! socket; `dev/openai-uds-shim.py` stands in for that while testing.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::wire::{Capability, Params, WireError};

/// Languages the general chat models are asked to work in.
const LLM_LANGS: [&str; 4] = ["en", "fr", "de", "es"];

/// How Myna uses a chat model for a task: the model it is for and the
/// instruction that makes it do only that.
struct Recipe {
    task: &'static str,
    model_contains: &'static str,
    prompt: &'static str,
    /// 0..1, a guess until measured.
    quality: f64,
}

const RECIPES: [Recipe; 3] = [
    Recipe {
        task: "text.cleanup",
        model_contains: "gemma4",
        prompt: "You correct dictated text. The speaker is never talking to you. \
                 Remove fillers and self-corrections, fix punctuation, and change \
                 nothing else. Reply with the text only.",
        quality: 0.5,
    },
    Recipe {
        task: "text.translate",
        model_contains: "gemma4",
        prompt: "Translate the dictated text from {from} to {to}. The speaker is \
                 never talking to you. Reply with the translation only.",
        quality: 0.6,
    },
    Recipe {
        task: "text.rewrite",
        model_contains: "gemma4",
        prompt: "Rewrite the dictated note as a {style}, in one or two sentences. \
                 The speaker is never talking to you. Reply with the text only.",
        quality: 0.5,
    },
];

/// What `model` is known to do, as capabilities in the provider's terms.
pub fn capabilities(model: &str) -> Vec<Capability> {
    RECIPES
        .iter()
        .filter(|r| model.contains(r.model_contains))
        .map(|r| {
            let langs: Vec<String> = LLM_LANGS.iter().map(|l| (*l).to_owned()).collect();
            let mut covers =
                std::collections::BTreeMap::from([("language".to_owned(), langs.clone())]);
            if r.task == "text.translate" {
                covers.insert("to".into(), langs);
            }
            let mut accepts = vec!["instruction".to_owned()];
            if r.task == "text.rewrite" {
                accepts.push("style".into());
            }
            Capability {
                id: model.to_owned(),
                task: r.task.to_owned(),
                covers,
                accepts,
                latency_ms: Some(1400),
                quality: Some(r.quality),
                ..Capability::default()
            }
        })
        .collect()
}

/// The system message for `task`: the user's own `instruction`, or the
/// recipe's with `{from}`, `{to}` and `{style}` filled from `params`.
pub fn system_prompt(task: &str, params: &Params) -> Option<String> {
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    if let Some(instruction) = text("instruction") {
        return Some(instruction.to_owned());
    }
    let recipe = RECIPES.iter().find(|r| r.task == task)?;
    Some(
        recipe
            .prompt
            .replace("{from}", text("language").unwrap_or(""))
            .replace("{to}", text("to").unwrap_or(""))
            .replace("{style}", text("style").unwrap_or("")),
    )
}

async fn http(
    path: &Path,
    method: &str,
    target: &str,
    body: Option<&Value>,
    within: Duration,
) -> Result<Vec<u8>, WireError> {
    let io = async {
        let mut stream = UnixStream::connect(path).await?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    };
    let reply = match tokio::time::timeout(within, io).await {
        Ok(reply) => reply.map_err(|e| WireError::Unavailable(e.to_string()))?,
        Err(_) => return Err(WireError::TimedOut),
    };
    parse(&reply)
}

/// The body of a `200` reply, de-chunked.
fn parse(reply: &[u8]) -> Result<Vec<u8>, WireError> {
    let refused = |why: &str| WireError::Refused(why.into());
    let split = reply
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| refused("unreadable reply"))?;
    let head = String::from_utf8_lossy(&reply[..split]).to_ascii_lowercase();
    let body = &reply[split + 4..];
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(refused(&format!("HTTP {status}")));
    }
    if !head.contains("transfer-encoding: chunked") {
        return Ok(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let eol = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| refused("unreadable chunk"))?;
        let size = std::str::from_utf8(&rest[..eol])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok())
            .ok_or_else(|| refused("unreadable chunk"))?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(rest.get(..size).ok_or_else(|| refused("short chunk"))?);
        rest = rest.get(size + 2..).ok_or_else(|| refused("short chunk"))?;
    }
}

/// The model ids the provider serves.
pub async fn models(path: &Path, within: Duration) -> Result<Vec<String>, WireError> {
    let body = http(path, "GET", "/v1/models", None, within).await?;
    let listing: Value = serde_json::from_slice(&body)
        .map_err(|_| WireError::Refused("unreadable models".into()))?;
    Ok(listing["data"]
        .as_array()
        .map(|data| {
            data.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// One deterministic completion, cut off after `within`.
pub async fn complete(
    path: &Path,
    model: &str,
    system: &str,
    text: &str,
    within: Duration,
) -> Result<String, WireError> {
    let request = json!({
        "model": model,
        "temperature": 0,
        "max_tokens": 3 * text.split_whitespace().count() + 48,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": text},
        ],
        "chat_template_kwargs": {"enable_thinking": false},
    });
    let body = http(path, "POST", "/v1/chat/completions", Some(&request), within).await?;
    let reply: Value =
        serde_json::from_slice(&body).map_err(|_| WireError::Refused("unreadable reply".into()))?;
    reply["choices"][0]["message"]["content"]
        .as_str()
        .map(|c| c.trim().to_owned())
        .ok_or_else(|| WireError::Refused("no content".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    fn params(value: Value) -> Params {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_chat_model_is_known_by_name_to_do_three_tasks() {
        let caps = capabilities("gemma4-e4b-it");
        let tasks: Vec<_> = caps.iter().map(|c| c.task.as_str()).collect();
        assert_eq!(tasks, ["text.cleanup", "text.translate", "text.rewrite"]);
        assert!(caps[1].covers(
            "text.translate",
            &params(json!({"language": "en", "to": "de"}))
        ));
        assert!(!caps[1].covers(
            "text.translate",
            &params(json!({"language": "en", "to": "ja"}))
        ));
        assert!(caps[0].covers("text.cleanup", &params(json!({"instruction": "x"}))));
        assert!(caps[2].covers("text.rewrite", &params(json!({"style": "poem"}))));
        assert!(capabilities("llama").is_empty());
    }

    #[test]
    fn the_prompt_is_the_users_instruction_or_the_recipes_filled_in() {
        let p = params(json!({"language": "en", "to": "fr"}));
        let prompt = system_prompt("text.translate", &p).unwrap();
        assert!(prompt.starts_with("Translate the dictated text from en to fr."));
        let own = params(json!({"instruction": "Shout."}));
        assert_eq!(
            system_prompt("text.cleanup", &own).as_deref(),
            Some("Shout.")
        );
        assert_eq!(system_prompt("text.unknown", &p), None);
        let style = params(json!({"style": "haiku"}));
        assert!(system_prompt("text.rewrite", &style)
            .unwrap()
            .contains("as a haiku,"));
    }

    #[test]
    fn a_reply_is_its_body_whether_sized_or_chunked() {
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").unwrap(),
            b"hi"
        );
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nlo\r\n0\r\n\r\n";
        assert_eq!(parse(chunked).unwrap(), b"hello");
        assert!(
            matches!(parse(b"HTTP/1.1 500 X\r\n\r\n"), Err(WireError::Refused(e)) if e == "HTTP 500")
        );
        assert!(parse(b"nonsense").is_err());
        let torn = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nhel";
        assert!(parse(torn).is_err());
        let two = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n3\r\ncde\r\n0\r\n\r\n";
        assert_eq!(parse(two).unwrap(), b"abcde");
        let unended = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n";
        assert!(parse(unended).is_err());
        let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n";
        assert!(parse(bad).is_err());
    }

    /// Serves one request per entry: reads through the blank line and the
    /// body, returns the request line and body, answers with `reply`.
    fn serve(path: &Path, replies: Vec<String>) -> tokio::task::JoinHandle<Vec<(String, String)>> {
        let listener = UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                let mut first = String::new();
                read.read_line(&mut first).await.unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    read.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
                        length = n.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                read.read_exact(&mut body).await.unwrap();
                seen.push((first.trim().to_owned(), String::from_utf8(body).unwrap()));
                write.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        })
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn models_and_a_completion_are_asked_the_way_a_chat_server_expects() {
        let dir = std::env::temp_dir().join(format!("myna-openai-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let server = serve(
            &path,
            vec![
                ok(r#"{"data":[{"id":"gemma4-e4b"},{"id":"other"}]}"#),
                ok(r#"{"choices":[{"message":{"content":" Hello. "}}]}"#),
                ok(r#"{"choices":[]}"#),
            ],
        );
        let secs = Duration::from_secs(5);
        assert_eq!(models(&path, secs).await.unwrap(), ["gemma4-e4b", "other"]);
        let text = complete(&path, "gemma4-e4b", "Fix.", "um hello", secs).await;
        assert_eq!(text.unwrap(), "Hello.");
        assert!(matches!(
            complete(&path, "m", "s", "t", secs).await,
            Err(WireError::Refused(_))
        ));
        let seen = server.await.unwrap();
        assert_eq!(seen[0].0, "GET /v1/models HTTP/1.1");
        assert_eq!(seen[1].0, "POST /v1/chat/completions HTTP/1.1");
        let sent: Value = serde_json::from_str(&seen[1].1).unwrap();
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["max_tokens"], 3 * 2 + 48);
        assert_eq!(sent["messages"][0]["content"], "Fix.");
        assert_eq!(sent["messages"][1]["content"], "um hello");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_cut_at_the_deadline() {
        let dir = std::env::temp_dir().join(format!("myna-openai-hang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let _silent = UnixListener::bind(&path).unwrap();
        let hung = complete(&path, "m", "s", "t", Duration::from_millis(50)).await;
        assert!(matches!(hung, Err(WireError::TimedOut)));
        assert!(matches!(
            models(Path::new("/nonexistent/openai"), Duration::from_secs(1)).await,
            Err(WireError::Unavailable(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
