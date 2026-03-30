use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::blocking::{Client, multipart};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::http::header::AUTHORIZATION;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket, connect};

#[derive(Debug, Clone)]
pub struct OpenAiRealtimeBackend {
    pub llm_api: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[allow(dead_code)]
pub trait RealtimeTransport {
    fn endpoint(&self) -> &str;
    fn connect(&mut self) -> Result<()>;
    fn send_session_update_model(&mut self, model: &str) -> Result<()>;
    fn send_audio_chunk(&mut self, pcm16: &[i16]) -> Result<()>;
    fn send_commit(&mut self) -> Result<()>;
    fn read_event_with_timeout(&mut self, timeout: Duration) -> Result<Option<serde_json::Value>>;
    fn close(&mut self) -> Result<()>;
}

type RealtimeSocket = WebSocket<MaybeTlsStream<TcpStream>>;

pub struct OpenAiRealtimeTransport {
    endpoint: String,
    api_key: String,
    socket: Option<RealtimeSocket>,
}

impl RealtimeTransport for OpenAiRealtimeTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn connect(&mut self) -> Result<()> {
        if self.socket.is_some() {
            return Ok(());
        }

        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .context("failed creating websocket request")?;

        request
            .headers_mut()
            .insert("OpenAI-Beta", HeaderValue::from_static("realtime=v1"));

        if !self.api_key.is_empty() {
            let bearer = format!("Bearer {}", self.api_key);
            let value = HeaderValue::from_str(&bearer)
                .context("failed constructing Authorization header")?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }

        let (socket, _) = connect(request).context("failed connecting realtime websocket")?;
        self.socket = Some(socket);

        Ok(())
    }

    fn send_session_update_model(&mut self, model: &str) -> Result<()> {
        self.send_json(serde_json::json!({
            "type": "session.update",
            "model": model,
            "session": {
                "model": model,
            },
        }))
    }

    fn send_audio_chunk(&mut self, pcm16: &[i16]) -> Result<()> {
        if pcm16.is_empty() {
            bail!("audio chunk cannot be empty");
        }

        let mut bytes = Vec::with_capacity(pcm16.len() * 2);
        for sample in pcm16 {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        self.send_json(serde_json::json!({
            "type": "input_audio_buffer.append",
            "audio": base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    }

    fn send_commit(&mut self) -> Result<()> {
        self.send_commit_with_final(false)
    }

    fn read_event_with_timeout(&mut self, timeout: Duration) -> Result<Option<serde_json::Value>> {
        let Some(socket) = self.socket.as_mut() else {
            bail!("transport is not connected");
        };

        set_socket_read_timeout(socket, Some(timeout))
            .context("failed setting websocket read timeout")?;

        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(error))
                if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
            {
                return Ok(None);
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ok(None);
            }
            Err(error) => {
                return Err(anyhow::Error::from(error).context("failed reading websocket event"));
            }
        };

        match message {
            Message::Text(text) => {
                let value: serde_json::Value = serde_json::from_str(text.as_str())
                    .context("failed parsing realtime websocket JSON event")?;
                Ok(Some(value))
            }
            Message::Binary(_) => Ok(None),
            Message::Ping(payload) => {
                socket
                    .send(Message::Pong(payload))
                    .context("failed replying to websocket ping")?;
                Ok(None)
            }
            Message::Pong(_) => Ok(None),
            Message::Close(_) => Ok(None),
            Message::Frame(_) => Ok(None),
        }
    }

    fn close(&mut self) -> Result<()> {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }

        Ok(())
    }
}

impl OpenAiRealtimeTransport {
    pub fn send_commit_with_final(&mut self, final_chunk: bool) -> Result<()> {
        let mut payload = serde_json::json!({
            "type": "input_audio_buffer.commit",
        });

        if final_chunk {
            payload["final"] = serde_json::json!(true);
        }

        self.send_json(payload)
    }

    fn send_json(&mut self, payload: serde_json::Value) -> Result<()> {
        let Some(socket) = self.socket.as_mut() else {
            bail!("transport is not connected");
        };

        socket
            .send(Message::Text(payload.to_string().into()))
            .context("failed sending realtime websocket message")?;

        Ok(())
    }
}

impl OpenAiRealtimeBackend {
    pub fn from_config(config: &toml::Value) -> Result<Self> {
        let llm_api =
            read_string(config, "server.llmApi")?.unwrap_or_else(|| "openai-realtime".to_string());
        let base_url = read_string(config, "server.baseUrl")?
            .unwrap_or_else(|| "http://127.0.0.1:8000/v1".to_string());
        let api_key = read_string(config, "server.apiKey")?.unwrap_or_default();
        let model = read_string(config, "server.model")?.unwrap_or_else(|| "voxtral".to_string());

        if llm_api != "openai-realtime" {
            bail!("unsupported server.llmApi={llm_api}; omni v1 only supports openai-realtime");
        }

        Ok(Self {
            llm_api,
            base_url,
            api_key,
            model,
        })
    }

    #[allow(dead_code)]
    pub fn ws_url(&self) -> String {
        let mut base = self.base_url.trim_end_matches('/').to_string();
        if base.starts_with("https://") {
            base = base.replacen("https://", "wss://", 1);
        } else if base.starts_with("http://") {
            base = base.replacen("http://", "ws://", 1);
        }

        format!("{base}/realtime?model={}", self.model)
    }

    #[allow(dead_code)]
    pub fn transcription_url(&self) -> String {
        format!(
            "{}/audio/transcriptions",
            self.base_url.trim_end_matches('/')
        )
    }

    #[allow(dead_code)]
    pub fn transcribe_wav_bytes(&self, wav: Vec<u8>) -> Result<String> {
        let file_part = multipart::Part::bytes(wav)
            .file_name("recording.wav")
            .mime_str("audio/wav")
            .context("failed setting wav mime type")?;

        let form = multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", file_part);

        let client = Client::new();
        let mut request = client.post(self.transcription_url()).multipart(form);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

        let response = request
            .send()
            .context("failed sending transcription request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<failed reading error body>".to_string());
            bail!("transcription request failed: {status} {body}");
        }

        let payload: serde_json::Value = response
            .json()
            .context("failed decoding transcription response JSON")?;

        extract_transcription_text(&payload)
    }

    #[allow(dead_code)]
    pub fn build_transport(&self) -> OpenAiRealtimeTransport {
        OpenAiRealtimeTransport {
            endpoint: self.ws_url(),
            api_key: self.api_key.clone(),
            socket: None,
        }
    }
}

fn read_string(config: &toml::Value, key: &str) -> Result<Option<String>> {
    let value = crate::config::get_value_by_key(config, key);
    let Some(value) = value else {
        return Ok(None);
    };

    let Some(as_string) = value.as_str() else {
        bail!("config key {key} must be a string");
    };

    Ok(Some(as_string.to_string()))
}

fn set_socket_read_timeout(
    socket: &mut RealtimeSocket,
    timeout: Option<Duration>,
) -> std::io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.get_mut().set_read_timeout(timeout),
        _ => Ok(()),
    }
}

fn extract_transcription_text(payload: &serde_json::Value) -> Result<String> {
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        return Ok(text.to_string());
    }

    if let Some(text) = payload.get("transcript").and_then(|v| v.as_str()) {
        return Ok(text.to_string());
    }

    if let Some(text) = payload
        .get("result")
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
    {
        return Ok(text.to_string());
    }

    bail!("transcription response did not include text/transcript field")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_openai_realtime_llm_api() {
        let mut config = crate::config::default_config();
        crate::config::set_value_by_key(
            &mut config,
            "server.llmApi",
            toml::Value::String("anthropic".into()),
        )
        .expect("set should work");

        let error = OpenAiRealtimeBackend::from_config(&config)
            .expect_err("non-openai-realtime llmApi should fail");

        assert!(error.to_string().contains("only supports openai-realtime"));
    }

    #[test]
    fn ws_url_uses_ws_scheme_and_model_query() {
        let config = crate::config::default_config();
        let backend = OpenAiRealtimeBackend::from_config(&config).expect("config should parse");

        assert_eq!(
            backend.ws_url(),
            "ws://127.0.0.1:8000/v1/realtime?model=voxtral"
        );
    }

    #[test]
    fn transport_send_requires_prior_connection() {
        let config = crate::config::default_config();
        let backend = OpenAiRealtimeBackend::from_config(&config).expect("config should parse");
        let mut transport = backend.build_transport();

        assert_eq!(
            transport.endpoint(),
            "ws://127.0.0.1:8000/v1/realtime?model=voxtral"
        );
        assert!(transport.send_audio_chunk(&[1, 2, 3]).is_err());
    }

    #[test]
    fn extracts_transcription_text_from_supported_shapes() {
        let a = serde_json::json!({"text": "hello"});
        assert_eq!(
            extract_transcription_text(&a).expect("should parse"),
            "hello"
        );

        let b = serde_json::json!({"transcript": "world"});
        assert_eq!(
            extract_transcription_text(&b).expect("should parse"),
            "world"
        );

        let c = serde_json::json!({"result": {"text": "nested"}});
        assert_eq!(
            extract_transcription_text(&c).expect("should parse"),
            "nested"
        );
    }
}
