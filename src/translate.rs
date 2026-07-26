//! Translation backends and the failover chain that walks them.
//!
//! Three providers ship in the binary. The engine tries them in the order
//! given by [`Config::providers`] (Google, then MyMemory, then DeepL by
//! default) and returns the first success, so a rate-limited or misconfigured
//! backend degrades into the next one instead of into an error.

use std::time::Duration;

use serde_json::Value;

use crate::config::{Config, Provider};

#[derive(Debug, Clone)]
pub struct Translation {
    pub text: String,
    /// Language the backend believed the input was in. "auto" when it did not
    /// say and we could not infer one.
    pub source_lang: String,
    pub provider: Provider,
}

#[derive(Debug, Clone)]
pub enum TranslateError {
    /// 429/456-style throttling. Worth trying the next provider immediately.
    RateLimited,
    /// The provider is listed but cannot run (no API key, text too long for
    /// its limits, language pair it does not offer).
    Unavailable(String),
    Http(u16),
    Network(String),
    /// Reached the server but could not make sense of what came back — the
    /// usual symptom of the unofficial Google endpoint serving a captcha page.
    BadResponse(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::RateLimited => write!(f, "rate limited"),
            TranslateError::Unavailable(why) => write!(f, "{why}"),
            TranslateError::Http(code) => write!(f, "HTTP {code}"),
            TranslateError::Network(msg) => write!(f, "network: {msg}"),
            TranslateError::BadResponse(msg) => write!(f, "bad response: {msg}"),
        }
    }
}

/// MyMemory rejects queries longer than this.
const MYMEMORY_MAX_BYTES: usize = 500;

/// MyMemory's magic source-language value: it detects the language itself and
/// reports the result in `responseData.detectedLanguage`.
const MYMEMORY_AUTODETECT: &str = "Autodetect";

pub struct Translator {
    agent: ureq::Agent,
}

impl Translator {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            // We inspect status codes ourselves so error bodies stay readable;
            // DeepL in particular explains refusals in the body.
            .http_status_as_error(false)
            .user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
            )
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Runs the configured chain and returns the first translation produced.
    /// On total failure every provider's reason comes back, so the bubble can
    /// say *why* rather than just "failed".
    pub fn translate(
        &self,
        text: &str,
        cfg: &Config,
    ) -> Result<Translation, Vec<(Provider, TranslateError)>> {
        let mut failures = Vec::new();

        for provider in cfg.active_providers() {
            match self.translate_with(provider, text, cfg) {
                Ok(translation) => return Ok(translation),
                Err(err) => failures.push((provider, err)),
            }
        }

        if failures.is_empty() {
            failures.push((
                Provider::Google,
                TranslateError::Unavailable("no providers enabled".into()),
            ));
        }
        Err(failures)
    }

    /// Asks one specific backend, bypassing the chain. Used by `--check` to
    /// report which providers are healthy right now.
    pub fn translate_with(
        &self,
        provider: Provider,
        text: &str,
        cfg: &Config,
    ) -> Result<Translation, TranslateError> {
        let source = cfg.source_lang.trim();
        let target = cfg.target_lang.trim();
        match provider {
            Provider::Google => self.google(text, source, target),
            Provider::MyMemory => self.mymemory(text, source, target, &cfg.mymemory_email),
            Provider::DeepL => self.deepl(text, source, target, &cfg.deepl_api_key),
        }
    }

    // -- Google ------------------------------------------------------------
    //
    // The `translate_a/single` endpoint is undocumented and unauthenticated.
    // It throttles per-IP and will happily answer a burst with HTML instead of
    // JSON, so every response is parsed defensively and a second host is tried
    // before giving up on it.

    fn google(
        &self,
        text: &str,
        source: &str,
        target: &str,
    ) -> Result<Translation, TranslateError> {
        let sl = if source.is_empty() { "auto" } else { source };
        let q = urlencoding::encode(text);
        let hosts = [
            format!(
                "https://translate.googleapis.com/translate_a/single\
                 ?client=gtx&sl={sl}&tl={target}&dt=t&q={q}"
            ),
            format!(
                "https://clients5.google.com/translate_a/single\
                 ?client=dict-chrome-ex&sl={sl}&tl={target}&dt=t&q={q}"
            ),
        ];

        let mut last = TranslateError::Network("no attempt made".into());
        for (idx, url) in hosts.iter().enumerate() {
            match self.get_text(url) {
                Ok(body) => match parse_google(&body) {
                    Ok((translated, detected)) => {
                        return Ok(Translation {
                            text: translated,
                            source_lang: if detected.is_empty() {
                                sl.to_string()
                            } else {
                                detected
                            },
                            provider: Provider::Google,
                        });
                    }
                    Err(err) => last = err,
                },
                Err(TranslateError::RateLimited) => {
                    last = TranslateError::RateLimited;
                    // One short backoff on the primary host; if it is still
                    // throttling, fall through to the mirror rather than make
                    // the user wait on a second sleep.
                    if idx == 0 {
                        std::thread::sleep(Duration::from_millis(500));
                        if let Ok(body) = self.get_text(url) {
                            if let Ok((translated, detected)) = parse_google(&body) {
                                return Ok(Translation {
                                    text: translated,
                                    source_lang: if detected.is_empty() {
                                        sl.to_string()
                                    } else {
                                        detected
                                    },
                                    provider: Provider::Google,
                                });
                            }
                        }
                    }
                }
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    // -- MyMemory ----------------------------------------------------------
    //
    // Needs an explicit language pair, but accepts the literal "Autodetect" as
    // the source and reports what it found in `detectedLanguage`. Anonymous
    // quota is small; `mymemory_email` raises it.

    fn mymemory(
        &self,
        text: &str,
        source: &str,
        target: &str,
        email: &str,
    ) -> Result<Translation, TranslateError> {
        if text.len() > MYMEMORY_MAX_BYTES {
            return Err(TranslateError::Unavailable(format!(
                "selection is {} bytes, MyMemory accepts {MYMEMORY_MAX_BYTES}",
                text.len()
            )));
        }
        let sl = if source.is_empty() || source == "auto" {
            MYMEMORY_AUTODETECT
        } else {
            source
        };
        if sl == target {
            return Err(TranslateError::Unavailable(
                "source and target are the same language".into(),
            ));
        }

        let mut url = format!(
            "https://api.mymemory.translated.net/get?q={}&langpair={}",
            urlencoding::encode(text),
            urlencoding::encode(&format!("{sl}|{target}")),
        );

        if !email.trim().is_empty() {
            url.push_str(&format!("&de={}", urlencoding::encode(email.trim())));
        }

        let body = self.get_text(&url)?;
        let json: Value = serde_json::from_str(&body)
            .map_err(|e| TranslateError::BadResponse(format!("not JSON ({e})")))?;

        // responseStatus arrives as a number or a string depending on the
        // failure mode, and quota exhaustion comes back as a 200 with an
        // explanatory string in responseDetails.
        let status = json
            .get("responseStatus")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        let details = json
            .get("responseDetails")
            .and_then(Value::as_str)
            .unwrap_or("");

        let quota_finished = json
            .get("quotaFinished")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if status == 429 || quota_finished || details.to_ascii_uppercase().contains("QUOTA") {
            return Err(TranslateError::RateLimited);
        }
        if status != 200 && status != 0 {
            return Err(TranslateError::BadResponse(if details.is_empty() {
                format!("status {status}")
            } else {
                details.to_string()
            }));
        }

        let translated = json
            .pointer("/responseData/translatedText")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if translated.is_empty() {
            return Err(TranslateError::BadResponse("empty translation".into()));
        }
        // MyMemory reports failures inside the text field too.
        if translated.starts_with("PLEASE SELECT TWO DISTINCT LANGUAGES")
            || translated.starts_with("INVALID LANGUAGE PAIR")
            || translated.starts_with("'")
                && translated.to_ascii_uppercase().contains("IS AN INVALID")
        {
            return Err(TranslateError::Unavailable(translated));
        }

        Ok(Translation {
            text: decode_html_entities(&translated),
            source_lang: json
                .pointer("/responseData/detectedLanguage")
                .and_then(Value::as_str)
                .unwrap_or(sl)
                .to_ascii_lowercase(),
            provider: Provider::MyMemory,
        })
    }

    // -- DeepL -------------------------------------------------------------

    fn deepl(
        &self,
        text: &str,
        source: &str,
        target: &str,
        key: &str,
    ) -> Result<Translation, TranslateError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(TranslateError::Unavailable("no API key configured".into()));
        }
        let Some(target_code) = deepl_target(target) else {
            return Err(TranslateError::Unavailable(format!(
                "DeepL has no target language '{target}'"
            )));
        };

        // Free keys carry a ":fx" suffix and live on a different host.
        let host = if key.ends_with(":fx") {
            "https://api-free.deepl.com"
        } else {
            "https://api.deepl.com"
        };

        let mut payload = serde_json::json!({
            "text": [text],
            "target_lang": target_code,
        });
        if source != "auto" && !source.is_empty() {
            if let Some(src) = deepl_source(source) {
                payload["source_lang"] = Value::String(src);
            }
        }
        let body = payload.to_string();

        let response = self
            .agent
            .post(format!("{host}/v2/translate"))
            .header("Authorization", format!("DeepL-Auth-Key {key}"))
            .header("Content-Type", "application/json")
            .send(body.as_str())
            .map_err(|e| TranslateError::Network(e.to_string()))?;

        let status = response.status().as_u16();
        let text_body = response
            .into_body()
            .read_to_string()
            .map_err(|e| TranslateError::Network(e.to_string()))?;

        match status {
            200 => {}
            // 456 is DeepL's dedicated "character quota exhausted".
            429 | 456 => return Err(TranslateError::RateLimited),
            403 => {
                return Err(TranslateError::Unavailable(
                    "DeepL rejected the API key".into(),
                ));
            }
            other => {
                let msg = serde_json::from_str::<Value>(&text_body)
                    .ok()
                    .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned));
                return Err(match msg {
                    Some(m) => TranslateError::BadResponse(format!("HTTP {other}: {m}")),
                    None => TranslateError::Http(other),
                });
            }
        }

        let json: Value = serde_json::from_str(&text_body)
            .map_err(|e| TranslateError::BadResponse(format!("not JSON ({e})")))?;
        let first = json
            .pointer("/translations/0")
            .ok_or_else(|| TranslateError::BadResponse("no translations in response".into()))?;
        let translated = first
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if translated.is_empty() {
            return Err(TranslateError::BadResponse("empty translation".into()));
        }

        Ok(Translation {
            text: translated,
            source_lang: first
                .get("detected_source_language")
                .and_then(Value::as_str)
                .unwrap_or(source)
                .to_ascii_lowercase(),
            provider: Provider::DeepL,
        })
    }

    fn get_text(&self, url: &str) -> Result<String, TranslateError> {
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| TranslateError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|e| TranslateError::Network(e.to_string()))?;

        match status {
            200 => Ok(body),
            429 | 503 => Err(TranslateError::RateLimited),
            other => Err(TranslateError::Http(other)),
        }
    }
}

/// Pulls the translated segments out of Google's untyped response.
///
/// Two shapes show up in practice:
///   `[[["hello","merhaba",...],[...]], null, "tr", ...]`
///   `{"sentences":[{"trans":"hello","orig":"merhaba"}],"src":"tr"}`
/// Anything else (notably an HTML captcha interstitial) becomes a
/// `BadResponse` rather than a panic.
fn parse_google(body: &str) -> Result<(String, String), TranslateError> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('<') {
        return Err(TranslateError::BadResponse(
            "endpoint returned HTML (throttled or captcha)".into(),
        ));
    }
    let json: Value = serde_json::from_str(body)
        .map_err(|e| TranslateError::BadResponse(format!("not JSON ({e})")))?;

    let (translated, detected) =
        if let Some(sentences) = json.get("sentences").and_then(Value::as_array) {
            let text = sentences
                .iter()
                .filter_map(|s| s.get("trans").and_then(Value::as_str))
                .collect::<String>();
            let src = json.get("src").and_then(Value::as_str).unwrap_or("");
            (text, src.to_string())
        } else if let Some(outer) = json.as_array() {
            let text = outer
                .first()
                .and_then(Value::as_array)
                .map(|segments| {
                    segments
                        .iter()
                        .filter_map(|seg| seg.get(0).and_then(Value::as_str))
                        .collect::<String>()
                })
                .unwrap_or_default();
            let src = outer.get(2).and_then(Value::as_str).unwrap_or("");
            (text, src.to_string())
        } else {
            return Err(TranslateError::BadResponse("unrecognised shape".into()));
        };

    if translated.trim().is_empty() {
        return Err(TranslateError::BadResponse("empty translation".into()));
    }
    Ok((translated, detected))
}

/// DeepL wants uppercase codes and is picky about the ones with variants.
fn deepl_target(code: &str) -> Option<String> {
    let base = code.split('-').next().unwrap_or(code).to_ascii_lowercase();
    let mapped = match base.as_str() {
        "en" => "EN-US",
        "pt" => "PT-PT",
        "zh" => "ZH",
        "bg" | "cs" | "da" | "de" | "el" | "es" | "et" | "fi" | "fr" | "hu" | "id" | "it"
        | "ja" | "ko" | "lt" | "lv" | "nb" | "nl" | "pl" | "ro" | "ru" | "sk" | "sl" | "sv"
        | "tr" | "uk" | "ar" => return Some(base.to_ascii_uppercase()),
        _ => return None,
    };
    Some(mapped.to_string())
}

fn deepl_source(code: &str) -> Option<String> {
    let base = code.split('-').next().unwrap_or(code).to_ascii_lowercase();
    match base.as_str() {
        "bg" | "cs" | "da" | "de" | "el" | "en" | "es" | "et" | "fi" | "fr" | "hu" | "id"
        | "it" | "ja" | "ko" | "lt" | "lv" | "nb" | "nl" | "pl" | "pt" | "ro" | "ru" | "sk"
        | "sl" | "sv" | "tr" | "uk" | "zh" | "ar" => Some(base.to_ascii_uppercase()),
        _ => None,
    }
}

/// MyMemory escapes a handful of entities in its output.
fn decode_html_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_google_array_shape() {
        let body = r#"[[["hello","merhaba",null,null,10]],null,"tr",null,null,null,null,[]]"#;
        let (text, src) = parse_google(body).unwrap();
        assert_eq!(text, "hello");
        assert_eq!(src, "tr");
    }

    #[test]
    fn joins_multi_segment_google_responses() {
        let body =
            r#"[[["Hello there. ","x",null,null,10],["How are you?","y",null,null,3]],null,"tr"]"#;
        let (text, _) = parse_google(body).unwrap();
        assert_eq!(text, "Hello there. How are you?");
    }

    #[test]
    fn parses_google_sentences_shape() {
        let body = r#"{"sentences":[{"trans":"hello","orig":"merhaba"}],"src":"tr"}"#;
        let (text, src) = parse_google(body).unwrap();
        assert_eq!(text, "hello");
        assert_eq!(src, "tr");
    }

    #[test]
    fn rejects_captcha_html() {
        let err = parse_google("<!DOCTYPE html><html>sorry</html>").unwrap_err();
        assert!(matches!(err, TranslateError::BadResponse(_)));
    }

    #[test]
    fn deepl_codes_are_mapped() {
        assert_eq!(deepl_target("en").as_deref(), Some("EN-US"));
        assert_eq!(deepl_target("tr").as_deref(), Some("TR"));
        assert_eq!(deepl_target("fa"), None);
    }

    #[test]
    fn deepl_is_skipped_without_a_key() {
        let mut cfg = Config::default();
        cfg.deepl_api_key.clear();
        assert_eq!(
            cfg.active_providers(),
            vec![Provider::Google, Provider::MyMemory]
        );
        cfg.deepl_api_key = "abc:fx".into();
        assert_eq!(cfg.active_providers().len(), 3);
    }
}
