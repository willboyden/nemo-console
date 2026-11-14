//! One ATOF line as typed, metadata-only fields.
//!
//! The structs below name every field the index keeps and nothing else. `serde` skips all other keys without
//! building a value, so a request line that carries megabytes of message history costs one pass over its bytes and
//! no allocation. Every string field is a [`Tok`]: only identifier-shaped text can come out of this module, which is
//! what keeps prompts, tool arguments and header values out of the index.

use std::borrow::Cow;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};

use super::lenient::{
    Count, Flag, Int, Num, Opt, Pairs, Payload, Product, Seq, Stamp, Tok, Truthy, skip_kinds,
};

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct RawEvent<'a> {
    #[serde(borrow)]
    pub kind: Tok<'a>,
    #[serde(borrow)]
    pub name: Tok<'a>,
    #[serde(borrow)]
    pub scope_category: Tok<'a>,
    #[serde(borrow)]
    pub category: Tok<'a>,
    #[serde(borrow)]
    pub uuid: Tok<'a>,
    #[serde(borrow)]
    pub parent_uuid: Tok<'a>,
    pub timestamp: Stamp,
    #[serde(borrow)]
    pub attributes: Seq<Tok<'a>, 8>,
    #[serde(borrow)]
    pub category_profile: Opt<Profile<'a>>,
    #[serde(borrow)]
    pub metadata: Opt<Meta<'a>>,
    #[serde(borrow)]
    pub data: Payload<Data<'a>>,
    #[serde(borrow)]
    pub data_schema: Opt<Schema<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Schema<'a> {
    #[serde(borrow)]
    pub name: Tok<'a>,
    #[serde(borrow)]
    pub version: Label<'a>,
}

/// An identifier, or a whole number as its text (`"version": 1` and `"version": "1"` are the same label).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Label<'a>(pub Option<Cow<'a, str>>);

impl<'de: 'a, 'a> Deserialize<'de> for Label<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Label<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(Label(super::ids::is_ident(v).then_some(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Label(
                    super::ids::is_ident(v).then(|| Cow::Owned(v.to_owned())),
                ))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(Label(Some(Cow::Owned(v.to_string()))))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Label(Some(Cow::Owned(v.to_string()))))
            }
            skip_kinds!(bool, float, unit, seq, map);
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Meta<'a> {
    #[serde(borrow)]
    pub agent_kind: Tok<'a>,
    #[serde(borrow)]
    pub session_id: Tok<'a>,
    #[serde(borrow)]
    pub nemo_relay_scope_role: Tok<'a>,
    #[serde(borrow)]
    pub llm_correlation_status: Tok<'a>,
    #[serde(borrow)]
    pub status: Tok<'a>,
    #[serde(borrow)]
    pub hook_event_name: Tok<'a>,
    #[serde(borrow, rename = "otel.status_code")]
    pub otel_status: Tok<'a>,
    #[serde(borrow, rename = "error.type")]
    pub error_type: Tok<'a>,
    #[serde(borrow, rename = "nemo_relay.log.severity")]
    pub severity: Tok<'a>,
    pub turn_index: Int,
    #[serde(rename = "http.response.status_code")]
    pub http_status: Int,
    #[serde(rename = "http.status_code")]
    pub http_status_legacy: Int,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Profile<'a> {
    #[serde(borrow)]
    pub model_name: Tok<'a>,
    #[serde(borrow)]
    pub tool_call_id: Tok<'a>,
    #[serde(borrow)]
    pub subtype: Tok<'a>,
    /// Seconds.
    pub time_to_first_chunk: Num,
    #[serde(borrow)]
    pub annotated_request: Opt<AnnReq<'a>>,
    #[serde(borrow)]
    pub annotated_response: Opt<AnnResp<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct AnnReq<'a> {
    #[serde(borrow)]
    pub model: Tok<'a>,
    pub stream: Flag,
    pub messages: Count,
    pub tools: Count,
    pub params: Opt<Params>,
    #[serde(borrow)]
    pub api_specific: Opt<ApiSpecific<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Params {
    pub max_tokens: Int,
    pub max_completion_tokens: Int,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ApiSpecific<'a> {
    #[serde(borrow)]
    pub reasoning_effort: Tok<'a>,
    #[serde(borrow)]
    pub output_config: Opt<Effort<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Effort<'a> {
    #[serde(borrow)]
    pub effort: Tok<'a>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct AnnResp<'a> {
    #[serde(borrow)]
    pub model: Tok<'a>,
    #[serde(borrow)]
    pub finish_reason: Tok<'a>,
    #[serde(borrow)]
    pub tool_calls: Seq<Named<'a>, 16>,
    pub usage: Opt<Usage>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Named<'a> {
    #[serde(borrow)]
    pub name: Tok<'a>,
}

/// Token counts in every spelling seen in captures: OpenAI (`prompt_tokens`, details objects), Anthropic
/// (`input_tokens`, `cache_*_input_tokens`) and Relay's own annotated form (`cache_read_tokens`, ...).
#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(default)]
pub struct Usage {
    pub prompt_tokens: Int,
    pub completion_tokens: Int,
    pub total_tokens: Int,
    pub input_tokens: Int,
    pub output_tokens: Int,
    pub cache_read_input_tokens: Int,
    pub cache_creation_input_tokens: Int,
    pub cache_read_tokens: Int,
    pub cache_write_tokens: Int,
    pub uncached_input_tokens: Int,
    pub reasoning_tokens: Int,
    pub prompt_tokens_details: Opt<PromptDetails>,
    pub completion_tokens_details: Opt<CompletionDetails>,
}

#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(default)]
pub struct PromptDetails {
    pub cached_tokens: Int,
    pub created_cache_tokens: Int,
    pub cache_creation_tokens: Int,
}

#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(default)]
pub struct CompletionDetails {
    pub reasoning_tokens: Int,
}

/// Token counts with one meaning whatever the API: `input` is everything the model read (cached, cache-writing and
/// plain prompt tokens together), `cached` and `cache_write` are parts of it, `total` is `input + output`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cached: Option<i64>,
    pub cache_write: Option<i64>,
    pub reasoning: Option<i64>,
    pub total: Option<i64>,
}

/// The most tokens one count may say. Far beyond any real call, and small enough that summing every count of the
/// largest index (5 million spans, several counts each) cannot overflow (audit F8).
pub const MAX_COUNT: i64 = 100_000_000_000;

fn nn(v: Int) -> Option<i64> {
    v.0.filter(|n| *n >= 0).map(|n| n.min(MAX_COUNT))
}

impl Usage {
    /// `anthropic_shaped`: this usage is Relay's annotated form of an Anthropic answer, where `prompt_tokens` leaves
    /// the cached tokens out although the key is spelled the OpenAI way.
    #[must_use]
    pub fn tokens(&self, anthropic_shaped: bool) -> Tokens {
        let reasoning = self
            .completion_tokens_details
            .get()
            .and_then(|d| nn(d.reasoning_tokens))
            .or(nn(self.reasoning_tokens));
        let details = self.prompt_tokens_details.get();
        let cached = details
            .and_then(|d| nn(d.cached_tokens))
            .or(nn(self.cache_read_input_tokens))
            .or(nn(self.cache_read_tokens));
        let cache_write = details
            .and_then(|d| nn(d.created_cache_tokens).or(nn(d.cache_creation_tokens)))
            .or(nn(self.cache_creation_input_tokens))
            .or(nn(self.cache_write_tokens));
        let (input, output) = if self.input_tokens.0.is_some() || self.output_tokens.0.is_some() {
            // Anthropic: input_tokens counts only what was neither read from nor written to the cache
            (
                nn(self.input_tokens).map(|i| {
                    i.saturating_add(cached.unwrap_or(0))
                        .saturating_add(cache_write.unwrap_or(0))
                }),
                nn(self.output_tokens),
            )
        } else {
            let parts_left_out = anthropic_shaped && self.uncached_input_tokens.0.is_none();
            (
                nn(self.prompt_tokens).map(|p| {
                    if parts_left_out {
                        p.saturating_add(cached.unwrap_or(0))
                            .saturating_add(cache_write.unwrap_or(0))
                    } else {
                        p
                    }
                }),
                nn(self.completion_tokens),
            )
        };
        let total = match (input, output) {
            (Some(i), Some(o)) => Some(i.saturating_add(o)),
            _ => nn(self.total_tokens),
        };
        Tokens {
            input,
            output,
            cached,
            cache_write,
            reasoning,
            total,
        }
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Data<'a> {
    /// The request body of an LLM start, or the content blocks of an Anthropic-style answer.
    #[serde(borrow)]
    pub content: Body<'a>,
    #[serde(borrow)]
    pub headers: Opt<Headers<'a>>,
    #[serde(borrow)]
    pub model: Tok<'a>,
    #[serde(borrow)]
    pub stop_reason: Tok<'a>,
    #[serde(borrow)]
    pub choices: Seq<Choice<'a>, 1>,
    pub usage: Opt<Usage>,
    // marks (the fields `parse_atof_line` allows, plus the routing and hook names of addendum 9.12)
    #[serde(borrow)]
    pub algorithm: Tok<'a>,
    #[serde(borrow)]
    pub selected_model: Tok<'a>,
    #[serde(borrow)]
    pub served_model: Tok<'a>,
    #[serde(borrow)]
    pub target: Tok<'a>,
    #[serde(borrow)]
    pub target_model: Tok<'a>,
    #[serde(borrow)]
    pub outcome: Tok<'a>,
    #[serde(borrow)]
    pub call_role: Tok<'a>,
    #[serde(borrow)]
    pub category: Tok<'a>,
    #[serde(borrow)]
    pub failure_kind: Tok<'a>,
    #[serde(borrow)]
    pub phase: Tok<'a>,
    #[serde(borrow)]
    pub event_type: Tok<'a>,
    #[serde(borrow)]
    pub provider: Tok<'a>,
    #[serde(borrow)]
    pub decision: Tok<'a>,
    #[serde(borrow)]
    pub route: Tok<'a>,
    #[serde(borrow)]
    pub tier: Tok<'a>,
    #[serde(borrow)]
    pub hook_event_name: Tok<'a>,
    #[serde(borrow)]
    pub tool_name: Tok<'a>,
    pub latency_ms: Num,
    pub call_index: Int,
    pub upstream_status: Int,
    pub chunk_index: Int,
    pub fallback_used: Flag,
    pub evidence: Opt<Evidence>,
    #[serde(borrow)]
    pub measurements: Seq<Meas<'a>, 8>,
    // tool results
    pub error: Truthy,
    pub is_error: Flag,
}

#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(default)]
pub struct Evidence {
    pub score: Num,
    pub threshold: Num,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Meas<'a> {
    #[serde(borrow)]
    pub name: Tok<'a>,
    #[serde(borrow)]
    pub kind: Tok<'a>,
    #[serde(borrow)]
    pub unit: Tok<'a>,
    pub value: Num,
    pub attributes: Pairs<8>,
}

/// Request headers: the client's product name and the session ids. No other header is read.
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Headers<'a> {
    #[serde(rename = "user-agent")]
    pub user_agent: Product,
    #[serde(borrow, rename = "x-opencode-session-id")]
    pub opencode_session: Tok<'a>,
    #[serde(borrow, rename = "x-session-id")]
    pub session: Tok<'a>,
    #[serde(borrow, rename = "x-claude-code-session-id")]
    pub claude_code_session: Tok<'a>,
    #[serde(borrow, rename = "x-nemo-relay-session-id")]
    pub relay_session: Tok<'a>,
}

impl Headers<'_> {
    /// The client's own session id: the first of the three headers a client may send.
    #[must_use]
    pub fn client_session(&self) -> Option<&str> {
        self.opencode_session
            .as_str()
            .or(self.session.as_str())
            .or(self.claude_code_session.as_str())
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Choice<'a> {
    #[serde(borrow)]
    pub finish_reason: Tok<'a>,
    #[serde(borrow)]
    pub message: Opt<ChoiceMessage<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ChoiceMessage<'a> {
    #[serde(borrow)]
    pub tool_calls: Seq<Call<'a>, 16>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Call<'a> {
    #[serde(borrow)]
    pub function: Opt<Named<'a>>,
}

/// The counted parts of a request body.
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ReqBody<'a> {
    #[serde(borrow)]
    pub model: Tok<'a>,
    pub messages: Count,
    pub tools: Count,
    pub stream: Flag,
    pub max_tokens: Int,
    pub max_completion_tokens: Int,
    #[serde(borrow)]
    pub reasoning_effort: Tok<'a>,
    #[serde(borrow)]
    pub output_config: Opt<Effort<'a>>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct Block<'a> {
    #[serde(borrow, rename = "type")]
    kind: Tok<'a>,
    #[serde(borrow)]
    name: Tok<'a>,
}

/// `data.content`: an object is a request body, a list is the content blocks of an answer (of which only the names
/// of the tools it asks for are kept), anything else is nothing.
#[derive(Debug, Default)]
pub enum Body<'a> {
    #[default]
    None,
    Request(ReqBody<'a>),
    Blocks {
        tool_names: Vec<Cow<'a, str>>,
        tool_uses: u32,
    },
}

impl<'de: 'a, 'a> Deserialize<'de> for Body<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> Visitor<'de> for V<'a> {
            type Value = Body<'a>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
                ReqBody::deserialize(de::value::MapAccessDeserializer::new(a)).map(Body::Request)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut tool_names = Vec::new();
                let mut tool_uses: u32 = 0;
                while let Some(b) = a.next_element::<Opt<Block<'a>>>()? {
                    let Some(b) = b.0 else { continue };
                    if b.kind.as_str() == Some("tool_use") {
                        tool_uses = tool_uses.saturating_add(1);
                        if tool_names.len() < 16
                            && let Some(n) = b.name.0
                        {
                            tool_names.push(n);
                        }
                    }
                }
                Ok(Body::Blocks {
                    tool_names,
                    tool_uses,
                })
            }
            skip_kinds!(bool, int, float, str, unit);
        }
        d.deserialize_any(V(std::marker::PhantomData))
    }
}

/// What a line is.
pub enum Parsed<'a> {
    /// Only white space.
    Blank,
    /// Not a JSON object the reader can follow (bad syntax, a duplicate key, nesting past the reader's limit).
    Malformed,
    Event(Box<RawEvent<'a>>),
}

/// Read one line. Never fails: a line that is not an event is [`Parsed::Malformed`].
#[must_use]
pub fn parse_line(line: &[u8]) -> Parsed<'_> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Parsed::Blank;
    }
    match serde_json::from_slice::<Opt<RawEvent<'_>>>(line) {
        Ok(Opt(Some(ev))) => Parsed::Event(Box::new(ev)),
        Ok(Opt(None)) | Err(_) => Parsed::Malformed,
    }
}

/// The request side of an LLM scope.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LlmRequest<'e> {
    pub model: Option<&'e str>,
    pub stream: Option<bool>,
    pub messages: Option<u32>,
    pub tool_defs: Option<u32>,
    pub max_tokens: Option<i64>,
    pub effort: Option<&'e str>,
    pub client: Option<&'e str>,
    pub client_session: Option<&'e str>,
    pub relay_session: Option<&'e str>,
}

/// The answer side of an LLM scope.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LlmResponse<'e> {
    pub served_model: Option<&'e str>,
    pub finish_reason: Option<&'e str>,
    pub tokens: Tokens,
    pub ttfc_us: Option<i64>,
    pub tool_calls: Option<u32>,
    pub tool_names: Vec<&'e str>,
}

impl<'a> RawEvent<'a> {
    #[must_use]
    pub fn is_mark(&self) -> bool {
        self.kind.as_str() == Some("mark")
            || (self.scope_category.0.is_none() && self.kind.as_str() != Some("scope"))
    }

    #[must_use]
    pub fn data(&self) -> Option<&Data<'a>> {
        self.data.value.as_ref()
    }

    fn request_body(&self) -> Option<&ReqBody<'a>> {
        match self.data()?.content {
            Body::Request(ref r) => Some(r),
            _ => None,
        }
    }

    /// What an LLM scope event says about the request. The raw body (`data.content`) is what the client sent, so it
    /// wins; Relay's annotated copy fills the gaps.
    #[must_use]
    pub fn llm_request(&self) -> LlmRequest<'_> {
        let profile = self.category_profile.get();
        let ann = profile.and_then(|p| p.annotated_request.get());
        let body = self.request_body();
        let headers = self.data().and_then(|d| d.headers.get());
        let params = ann.and_then(|a| a.params.get());
        let api = ann.and_then(|a| a.api_specific.get());
        LlmRequest {
            model: profile
                .and_then(|p| p.model_name.as_str())
                .or(ann.and_then(|a| a.model.as_str()))
                .or(body.and_then(|b| b.model.as_str())),
            stream: body
                .and_then(|b| b.stream.0)
                .or(ann.and_then(|a| a.stream.0)),
            messages: body
                .and_then(|b| b.messages.0)
                .or(ann.and_then(|a| a.messages.0)),
            tool_defs: body.and_then(|b| b.tools.0).or(ann.and_then(|a| a.tools.0)),
            max_tokens: body
                .and_then(|b| b.max_tokens.0.or(b.max_completion_tokens.0))
                .or(params.and_then(|p| p.max_tokens.0.or(p.max_completion_tokens.0))),
            effort: body
                .and_then(|b| {
                    b.reasoning_effort
                        .as_str()
                        .or(b.output_config.get().and_then(|o| o.effort.as_str()))
                })
                .or(api.and_then(|a| {
                    a.reasoning_effort
                        .as_str()
                        .or(a.output_config.get().and_then(|o| o.effort.as_str()))
                })),
            client: headers.and_then(|h| h.user_agent.0.as_deref()),
            client_session: headers.and_then(Headers::client_session),
            relay_session: headers.and_then(|h| h.relay_session.as_str()),
        }
    }

    /// What an LLM scope event says about the answer.
    #[must_use]
    pub fn llm_response(&self, anthropic_api: bool) -> LlmResponse<'_> {
        let profile = self.category_profile.get();
        let ann = profile.and_then(|p| p.annotated_response.get());
        let d = self.data();
        let choice = d.and_then(|d| d.choices.items.first());
        let tokens = match (
            d.and_then(|d| d.usage.get()),
            ann.and_then(|a| a.usage.get()),
        ) {
            (Some(raw), _) => raw.tokens(false),
            (None, Some(a)) => a.tokens(anthropic_api),
            (None, None) => Tokens::default(),
        };
        let mut tool_names: Vec<&str> = Vec::new();
        let mut tool_calls: Option<u32> = None;
        if let Some(a) = ann.filter(|a| a.tool_calls.total > 0) {
            tool_calls = Some(a.tool_calls.total);
            tool_names.extend(a.tool_calls.items.iter().filter_map(|c| c.name.as_str()));
        } else if let Some(Body::Blocks {
            tool_names: names,
            tool_uses,
        }) = d.map(|d| &d.content)
        {
            tool_calls = Some(*tool_uses);
            tool_names.extend(names.iter().map(|n| &**n));
        } else if let Some(m) = choice.and_then(|c| c.message.get()) {
            tool_calls = Some(m.tool_calls.total);
            tool_names.extend(
                m.tool_calls
                    .items
                    .iter()
                    .filter_map(|c| c.function.get().and_then(|f| f.name.as_str())),
            );
        } else if ann.is_some() || choice.is_some() {
            tool_calls = Some(0);
        }
        #[allow(clippy::cast_possible_truncation)]
        let ttfc_us = profile
            .and_then(|p| p.time_to_first_chunk.0)
            .filter(|s| *s >= 0.0 && *s < 1e9)
            .map(|s| (s * 1e6).round() as i64);
        LlmResponse {
            served_model: d
                .and_then(|d| d.model.as_str())
                .or(ann.and_then(|a| a.model.as_str())),
            // Relay's own word first (complete / length / tool_use): one vocabulary for both APIs
            finish_reason: ann
                .and_then(|a| a.finish_reason.as_str())
                .or(d.and_then(|d| d.stop_reason.as_str()))
                .or(choice.and_then(|c| c.finish_reason.as_str())),
            tokens,
            ttfc_us,
            tool_calls,
            tool_names,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(text: &str) -> Box<RawEvent<'_>> {
        match parse_line(text.as_bytes()) {
            Parsed::Event(e) => e,
            _ => panic!("not an event: {text}"),
        }
    }

    #[test]
    fn lines_that_are_not_events() {
        assert!(matches!(parse_line(b""), Parsed::Blank));
        assert!(matches!(parse_line(b" \t\r"), Parsed::Blank));
        for bad in [
            &b"not json"[..],
            b"{\"kind\":",
            b"[1,2]",
            b"\"text\"",
            b"7",
            b"null",
            b"{\"kind\":\"mark\",\"kind\":\"scope\"}",
            b"{\"a\":1} trailing",
            b"\xff\xfe",
        ] {
            assert!(matches!(parse_line(bad), Parsed::Malformed), "{bad:?}");
        }
        // deeper than the JSON reader follows inside a field that is read: malformed, not a crash
        let deep = format!(
            r#"{{"metadata":{}1{}}}"#,
            "[".repeat(5000),
            "]".repeat(5000)
        );
        let _ = parse_line(deep.as_bytes());
    }

    #[test]
    fn usage_means_the_same_in_both_spellings() {
        let openai: Usage = serde_json::from_str(
            r#"{"completion_tokens":527,"completion_tokens_details":{"reasoning_tokens":425},"prompt_tokens":92263,
                "prompt_tokens_details":{"cached_tokens":4000,"created_cache_tokens":10},"total_tokens":92790}"#,
        )
        .unwrap();
        assert_eq!(
            openai.tokens(false),
            Tokens {
                input: Some(92263),
                output: Some(527),
                cached: Some(4000),
                cache_write: Some(10),
                reasoning: Some(425),
                total: Some(92790)
            }
        );
        let anthropic: Usage = serde_json::from_str(
            r#"{"cache_creation_input_tokens":100,"cache_read_input_tokens":17600,"input_tokens":3269,"output_tokens":830}"#,
        )
        .unwrap();
        assert_eq!(
            anthropic.tokens(false),
            Tokens {
                input: Some(3269 + 17600 + 100),
                output: Some(830),
                cached: Some(17600),
                cache_write: Some(100),
                reasoning: None,
                total: Some(3269 + 17600 + 100 + 830)
            }
        );
        // Relay's annotated copy of an Anthropic answer leaves the cached tokens out of prompt_tokens
        let annotated: Usage = serde_json::from_str(
            r#"{"cache_read_tokens":17600,"completion_tokens":830,"prompt_tokens":3269,"total_tokens":4099}"#,
        )
        .unwrap();
        assert_eq!(annotated.tokens(true).input, Some(20869));
        assert_eq!(annotated.tokens(false).input, Some(3269));
        // ... and of an OpenAI answer says so with uncached_input_tokens
        let annotated: Usage = serde_json::from_str(
            r#"{"cache_read_tokens":50,"completion_tokens":5,"prompt_tokens":459,"total_tokens":464,"uncached_input_tokens":409}"#,
        )
        .unwrap();
        assert_eq!(annotated.tokens(true).input, Some(459));
        // nothing known stays unknown; a negative count is not a count
        assert_eq!(Usage::default().tokens(false), Tokens::default());
        let odd: Usage = serde_json::from_str(
            r#"{"prompt_tokens":-4,"completion_tokens":"7","total_tokens":9}"#,
        )
        .unwrap();
        assert_eq!(
            odd.tokens(false),
            Tokens {
                total: Some(9),
                ..Tokens::default()
            }
        );
    }

    #[test]
    fn llm_fields_from_an_openai_pair() {
        let start = ev(
            r#"{"kind":"scope","scope_category":"start","category":"llm","name":"openai.chat_completions",
            "uuid":"01a0f1e0-03ef-791f-82e5-fb77b5e3057d","parent_uuid":"01a0f1e0-03ed-74b0-828f-00997d7ced48",
            "timestamp":"2026-11-02T12:26:36.318081+00:00","attributes":["streaming","two words"],
            "category_profile":{"annotated_request":{"messages":[{"role":"user","content":"SECRET"}],"model":"local-coder",
              "params":{"max_tokens":16},"api_specific":{"reasoning_effort":"high"}},"model_name":"route-a"},
            "data":{"content":{"max_completion_tokens":32,"messages":[{"role":"system","content":"SECRET"},{"role":"user","content":"SECRET"}],
              "model":"local-coder","stream":true,"tools":[{"type":"function"},{"type":"function"},{"type":"function"}]},
              "headers":{"user-agent":"opencode/2.0.0","x-session-id":"ses_b","x-opencode-session-id":"ses_a",
                         "authorization":"Bearer SECRET","x-nemo-relay-session-id":"session-1"}},
            "metadata":{"gateway_path":"/v1/chat/completions","llm_correlation_status":"agent_fallback"}}"#,
        );
        assert!(!start.is_mark());
        assert_eq!(start.attributes.items.len(), 2);
        assert_eq!(start.attributes.items[0].as_str(), Some("streaming"));
        assert_eq!(start.attributes.items[1].as_str(), None);
        assert_eq!(
            start.llm_request(),
            LlmRequest {
                model: Some("route-a"),
                stream: Some(true),
                messages: Some(2),
                tool_defs: Some(3),
                max_tokens: Some(32),
                effort: Some("high"),
                client: Some("opencode"),
                client_session: Some("ses_a"),
                relay_session: Some("session-1"),
            }
        );
        let end = ev(
            r#"{"kind":"scope","scope_category":"end","category":"llm","name":"openai.chat_completions",
            "uuid":"01a0f1e0-03ef-791f-82e5-fb77b5e3057d","timestamp":1793624801060550,
            "category_profile":{"annotated_response":{"finish_reason":"tool_use","model":"local-small",
              "tool_calls":[{"name":"grep","arguments":{"pattern":"SECRET"}},{"name":"two words"}],
              "usage":{"completion_tokens":5,"prompt_tokens":459,"total_tokens":464}},"model_name":"route-a","time_to_first_chunk":0.649454507},
            "data":{"choices":[{"finish_reason":"tool_calls","index":0,"message":{"content":"SECRET","tool_calls":[{"function":{"name":"grep","arguments":"SECRET"}}]}}],
              "model":"local-small","usage":{"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":2},"prompt_tokens":459,"total_tokens":464}},
            "metadata":{"otel.status_code":"OK","error.type":"internal_error","http.response.status_code":502}}"#,
        );
        assert_eq!(end.timestamp.0, Some(1_793_624_801_060_550));
        let r = end.llm_response(false);
        assert_eq!(r.served_model, Some("local-small"));
        assert_eq!(r.finish_reason, Some("tool_use"));
        assert_eq!(r.tokens.input, Some(459));
        assert_eq!(r.tokens.reasoning, Some(2));
        assert_eq!(r.ttfc_us, Some(649_455));
        assert_eq!(r.tool_calls, Some(2));
        assert_eq!(r.tool_names, vec!["grep"]);
        let md = end.metadata.get().unwrap();
        assert_eq!(md.otel_status.as_str(), Some("OK"));
        assert_eq!(md.error_type.as_str(), Some("internal_error"));
        assert_eq!(md.http_status.0, Some(502));
    }

    #[test]
    fn llm_fields_from_an_anthropic_answer_and_a_mark() {
        let end = ev(
            r#"{"kind":"scope","scope_category":"end","category":"llm","name":"anthropic.messages","uuid":"u-1",
            "timestamp":"2026-11-01T12:02:44.755107+00:00","category_profile":{"model_name":"auto-local"},
            "data":{"content":[{"type":"text","text":"SECRET"},{"id":"x","input":{"command":"SECRET"},"name":"Bash","type":"tool_use"},
                               "stray",{"type":"tool_use","name":"has spaces"}],
              "model":"local-small","stop_reason":"tool_use","usage":{"input_tokens":20892,"output_tokens":175}}}"#,
        );
        let r = end.llm_response(true);
        assert_eq!(r.finish_reason, Some("tool_use"));
        assert_eq!(
            (r.tokens.input, r.tokens.output, r.tokens.total),
            (Some(20892), Some(175), Some(21067))
        );
        assert_eq!(r.tool_calls, Some(2));
        assert_eq!(r.tool_names, vec!["Bash"]);
        assert!(end.data.present);
        let mark = ev(
            r#"{"kind":"mark","name":"switchyard.routing.llm_tokens","uuid":"01a0f1e0-000e-73dd-8dcf-06a4bcc14aca",
            "parent_uuid":"01a0f1e0-0004-733d-840b-7c9087b09e13","timestamp":"2026-11-01T12:11:44.755107+00:00",
            "data_schema":{"name":"nemo.relay.metric_measurements","version":1},
            "data":{"measurements":[{"attributes":{"call_role":"routing","target_model":"local-small","token_type":"input"},
              "description":"SECRET words","kind":"counter","name":"switchyard.routing.llm_tokens","unit":"{token}","value":5396}]},
            "metadata":{"nemo_relay.log.severity":"info","session_id":null}}"#,
        );
        assert!(mark.is_mark());
        let m = &mark.data().unwrap().measurements.items[0];
        assert_eq!(m.value.0, Some(5396.0));
        assert_eq!(m.unit.as_str(), None); // "{token}" is not identifier-shaped
        assert_eq!(m.attributes.0.len(), 3);
        let schema = mark.data_schema.get().unwrap();
        assert_eq!(schema.version.0.as_deref(), Some("1"));
        assert_eq!(mark.metadata.get().unwrap().severity.as_str(), Some("info"));
        // null data, null profile, null metadata
        let bare = ev(
            r#"{"kind":"scope","scope_category":"start","category":"agent","name":"gateway","uuid":"u-2","timestamp":"2026-11-02T12:18:43.777083+00:00","data":null,"category_profile":null,"metadata":null,"attributes":[]}"#,
        );
        assert!(!bare.data.present && bare.category_profile.get().is_none());
        assert_eq!(bare.llm_request(), LlmRequest::default());
        assert_eq!(bare.llm_response(false), LlmResponse::default());
    }
}
