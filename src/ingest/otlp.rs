use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use flate2::read::GzDecoder;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest as PbExportTraceServiceRequest;
use prost::Message;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::io::Read;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    http::error::ApiError,
    ingest::batch::{BatchIngestRequest, ObservationIngest, TraceIngest},
    state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelExportTraceServiceRequest {
    resource_spans: Vec<OtelResourceSpan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelResourceSpan {
    #[serde(default)]
    resource: Option<OtelResource>,
    #[serde(default)]
    scope_spans: Vec<OtelScopeSpan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelResource {
    #[serde(default)]
    attributes: Vec<OtelKeyValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelScopeSpan {
    #[serde(default)]
    spans: Vec<OtelSpan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelSpan {
    trace_id: String,
    span_id: String,
    #[serde(default)]
    parent_span_id: Option<String>,
    name: String,
    #[serde(default)]
    start_time_unix_nano: Option<String>,
    #[serde(default)]
    end_time_unix_nano: Option<String>,
    #[serde(default)]
    attributes: Vec<OtelKeyValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelKeyValue {
    key: String,
    #[serde(default)]
    value: Option<OtelAnyValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelAnyValue {
    #[serde(default)]
    string_value: Option<String>,
    #[serde(default)]
    int_value: Option<String>,
    #[serde(default)]
    double_value: Option<f64>,
    #[serde(default)]
    bool_value: Option<bool>,
    #[serde(default)]
    array_value: Option<OtelArrayValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OtelArrayValue {
    #[serde(default)]
    values: Vec<OtelAnyValue>,
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let hex_val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn otel_trace_id_to_uuid(trace_id: &str) -> Option<Uuid> {
    let raw = decode_hex(trace_id)?;
    if raw.len() != 16 {
        return None;
    }
    Uuid::from_slice(&raw).ok()
}

fn otel_span_id_to_uuid(span_id: &str) -> Option<Uuid> {
    let raw = decode_hex(span_id)?;
    if raw.len() != 8 {
        return None;
    }
    let mut padded = [0u8; 16];
    padded[8..].copy_from_slice(&raw);
    Uuid::from_slice(&padded).ok()
}

fn unix_nano_to_datetime(v: &Option<String>) -> Option<DateTime<Utc>> {
    let s = v.as_deref()?.trim();
    let nanos: i128 = s.parse().ok()?;
    if nanos <= 0 {
        return None;
    }
    let secs = (nanos / 1_000_000_000) as i64;
    let sub_nanos = (nanos % 1_000_000_000) as u32;
    Utc.timestamp_opt(secs, sub_nanos).single()
}

fn otel_any_value_to_json(v: &OtelAnyValue) -> JsonValue {
    if let Some(s) = &v.string_value {
        return JsonValue::String(s.clone());
    }
    if let Some(s) = &v.int_value {
        if let Ok(i) = s.parse::<i64>() {
            return JsonValue::Number(i.into());
        }
        return JsonValue::String(s.clone());
    }
    if let Some(f) = v.double_value {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return JsonValue::Number(n);
        }
    }
    if let Some(b) = v.bool_value {
        return JsonValue::Bool(b);
    }
    if let Some(arr) = &v.array_value {
        return JsonValue::Array(arr.values.iter().map(otel_any_value_to_json).collect());
    }
    JsonValue::Null
}

fn otel_value_to_json(v: &Option<OtelAnyValue>) -> JsonValue {
    v.as_ref()
        .map(otel_any_value_to_json)
        .unwrap_or(JsonValue::Null)
}

fn attributes_to_map(attrs: &[OtelKeyValue]) -> serde_json::Map<String, JsonValue> {
    let mut m = serde_json::Map::new();
    for kv in attrs {
        m.insert(kv.key.clone(), otel_value_to_json(&kv.value));
    }
    m
}

fn extract_string_attr(attrs: &[OtelKeyValue], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.string_value.clone())
}

fn extract_array_string_attr(attrs: &[OtelKeyValue], key: &str) -> Option<Vec<String>> {
    let v = attrs
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())?
        .array_value
        .as_ref()?;

    let mut out = Vec::with_capacity(v.values.len());
    for item in &v.values {
        if let Some(s) = &item.string_value {
            out.push(s.clone());
        }
    }
    Some(out)
}

fn extract_prefixed_map(
    attrs: &[OtelKeyValue],
    prefix: &str,
) -> serde_json::Map<String, JsonValue> {
    let mut out = serde_json::Map::new();
    for kv in attrs {
        if let Some(rest) = kv.key.strip_prefix(prefix) {
            if !rest.is_empty() {
                if let Some(v) = &kv.value {
                    out.insert(rest.to_string(), otel_any_value_to_json(v));
                }
            }
        }
    }
    out
}

fn parse_usage_details(
    attrs: &[OtelKeyValue],
) -> (Option<i64>, Option<i64>, Option<i64>, Option<JsonValue>) {
    let raw = match extract_string_attr(attrs, "langfuse.observation.usage_details") {
        Some(s) => s,
        None => return (None, None, None, None),
    };
    let v: JsonValue = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return (None, None, None, None),
    };
    let prompt = v.get("promptTokens").and_then(|x| x.as_i64());
    let completion = v.get("completionTokens").and_then(|x| x.as_i64());
    let total = v.get("totalTokens").and_then(|x| x.as_i64());
    let usage = Some(serde_json::json!({
        "input": prompt.unwrap_or(0),
        "output": completion.unwrap_or(0),
        "total": total.unwrap_or(0)
    }));
    (completion, prompt, total, usage)
}

fn is_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("gzip"))
}

fn content_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
}

fn ungzip_if_needed(headers: &HeaderMap, body: Bytes) -> Result<Vec<u8>, ApiError> {
    if !is_gzip(headers) {
        return Ok(body.to_vec());
    }
    let mut decoder = GzDecoder::new(body.as_ref());
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| ApiError::BadRequest(format!("gzip decode failed: {e}")))?;
    Ok(out)
}

fn map_otel_to_batches(
    state: &AppState,
    payload: OtelExportTraceServiceRequest,
) -> Result<Vec<BatchIngestRequest>, ApiError> {
    let default_project_id = state.default_project_id.as_ref().to_string();
    let mut per_trace: std::collections::BTreeMap<Uuid, Vec<ObservationIngest>> =
        std::collections::BTreeMap::new();
    let mut trace_first_ts: std::collections::BTreeMap<Uuid, DateTime<Utc>> =
        std::collections::BTreeMap::new();
    let mut trace_acc: std::collections::BTreeMap<Uuid, TraceIngest> =
        std::collections::BTreeMap::new();

    for rs in payload.resource_spans {
        let resource_attrs = rs.resource.as_ref().map(|r| &r.attributes);
        for ss in rs.scope_spans {
            for span in ss.spans {
                let trace_id = match otel_trace_id_to_uuid(&span.trace_id) {
                    Some(v) => v,
                    None => continue,
                };
                let span_uuid = match otel_span_id_to_uuid(&span.span_id) {
                    Some(v) => v,
                    None => continue,
                };
                let parent_uuid = match span.parent_span_id.as_deref() {
                    Some(p) if !p.is_empty() && p != "0000000000000000" => otel_span_id_to_uuid(p),
                    _ => None,
                };

                let start_time = unix_nano_to_datetime(&span.start_time_unix_nano);
                let end_time = unix_nano_to_datetime(&span.end_time_unix_nano);

                if let Some(st) = start_time {
                    trace_first_ts
                        .entry(trace_id)
                        .and_modify(|cur| {
                            if st < *cur {
                                *cur = st
                            }
                        })
                        .or_insert(st);
                }

                let obs_type = extract_string_attr(&span.attributes, "langfuse.observation.type")
                    .map(|s| s.to_uppercase());

                let model = extract_string_attr(&span.attributes, "langfuse.generation.model")
                    .or_else(|| extract_string_attr(&span.attributes, "gen_ai.request.model"));

                let input = extract_string_attr(&span.attributes, "langfuse.observation.input")
                    .and_then(|s| {
                        serde_json::from_str::<JsonValue>(&s)
                            .ok()
                            .or(Some(JsonValue::String(s)))
                    });

                let output = extract_string_attr(&span.attributes, "langfuse.observation.output")
                    .and_then(|s| {
                        serde_json::from_str::<JsonValue>(&s)
                            .ok()
                            .or(Some(JsonValue::String(s)))
                    });

                let trace_name = extract_string_attr(&span.attributes, "langfuse.trace.name");
                let user_id = extract_string_attr(&span.attributes, "user.id")
                    .or_else(|| extract_string_attr(&span.attributes, "langfuse.user.id"))
                    .or_else(|| extract_string_attr(&span.attributes, "enduser.id"));
                let session_id = extract_string_attr(&span.attributes, "session.id");
                let tags = extract_array_string_attr(&span.attributes, "langfuse.trace.tags");
                let trace_meta = extract_prefixed_map(&span.attributes, "langfuse.trace.metadata.");

                let span_trace_name = trace_name.clone().or_else(|| {
                    if parent_uuid.is_none() {
                        Some(span.name.clone())
                    } else {
                        None
                    }
                });

                trace_acc
                    .entry(trace_id)
                    .and_modify(|t| {
                        if t.name.is_none() {
                            t.name = span_trace_name.clone();
                        }
                        if t.userId.is_none() {
                            t.userId = user_id.clone();
                        }
                        if t.session_id.is_none() {
                            t.session_id = session_id.clone();
                        }
                        if t.tags.is_empty() {
                            if let Some(tags) = &tags {
                                t.tags = tags.clone();
                            }
                        }
                        if let Some(m) = &mut t.metadata {
                            if let JsonValue::Object(obj) = m {
                                for (k, v) in trace_meta.clone() {
                                    obj.insert(k, v);
                                }
                            }
                        } else if !trace_meta.is_empty() {
                            t.metadata = Some(JsonValue::Object(trace_meta.clone()));
                        }
                    })
                    .or_insert_with(|| TraceIngest {
                        id: trace_id,
                        timestamp: None,
                        name: span_trace_name.clone(),
                        input: None,
                        output: None,
                        session_id: session_id.clone(),
                        release: None,
                        version: None,
                        userId: user_id.clone(),
                        metadata: if trace_meta.is_empty() {
                            None
                        } else {
                            Some(JsonValue::Object(trace_meta.clone()))
                        },
                        tags: tags.unwrap_or_default(),
                        public: None,
                        environment: Some("default".to_string()),
                        externalId: None,
                        bookmarked: None,
                        latency: None,
                        totalCost: None,
                        projectId: Some(default_project_id.clone()),
                    });

                let mut meta = attributes_to_map(&span.attributes);
                if let Some(rattrs) = resource_attrs {
                    meta.insert(
                        "otel.resource".to_string(),
                        JsonValue::Object(attributes_to_map(rattrs)),
                    );
                }

                let (completion_tokens, prompt_tokens, total_tokens, usage_json) =
                    parse_usage_details(&span.attributes);

                let obs_latency = match (start_time, end_time) {
                    (Some(st), Some(et)) => {
                        let secs = (et - st).num_milliseconds() as f64 / 1000.0;
                        if secs.is_finite() && secs >= 0.0 {
                            Some(secs)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                let level = extract_string_attr(&span.attributes, "langfuse.observation.level")
                    .map(|s| s.to_uppercase())
                    .or_else(|| {
                        extract_string_attr(&span.attributes, "otel.status_code").and_then(|s| {
                            if s == "ERROR" || s == "2" {
                                Some("ERROR".to_string())
                            } else {
                                None
                            }
                        })
                    });

                let obs = ObservationIngest {
                    id: span_uuid,
                    traceId: trace_id,
                    r#type: obs_type,
                    name: Some(span.name.clone()),
                    startTime: start_time,
                    endTime: end_time,
                    completionStartTime: None,
                    model,
                    modelParameters: None,
                    input,
                    output,
                    usage: usage_json,
                    level,
                    statusMessage: extract_string_attr(
                        &span.attributes,
                        "langfuse.observation.status_message",
                    ),
                    parentObservationId: parent_uuid,
                    promptId: None,
                    promptName: None,
                    promptVersion: None,
                    modelId: None,
                    inputPrice: None,
                    outputPrice: None,
                    totalPrice: None,
                    calculatedInputCost: None,
                    calculatedOutputCost: None,
                    calculatedTotalCost: None,
                    latency: obs_latency,
                    timeToFirstToken: None,
                    completionTokens: completion_tokens,
                    promptTokens: prompt_tokens,
                    totalTokens: total_tokens,
                    unit: None,
                    metadata: Some(JsonValue::Object(meta)),
                    environment: None,
                    projectId: Some(default_project_id.clone()),
                };

                per_trace.entry(trace_id).or_default().push(obs);
            }
        }
    }

    let mut out = Vec::with_capacity(per_trace.len());
    for (trace_id, observations) in per_trace {
        let timestamp = trace_first_ts.get(&trace_id).cloned();
        let mut trace = trace_acc.remove(&trace_id).unwrap_or(TraceIngest {
            id: trace_id,
            timestamp: None,
            name: None,
            input: None,
            output: None,
            session_id: None,
            release: None,
            version: None,
            userId: None,
            metadata: None,
            tags: vec![],
            public: None,
            environment: Some("default".to_string()),
            externalId: None,
            bookmarked: None,
            latency: None,
            totalCost: None,
            projectId: Some(default_project_id.clone()),
        });
        trace.timestamp = timestamp;
        out.push(BatchIngestRequest {
            trace: Some(trace),
            observations,
        });
    }
    Ok(out)
}

fn pb_to_otel_json(payload: PbExportTraceServiceRequest) -> OtelExportTraceServiceRequest {
    let resource_spans = payload
        .resource_spans
        .into_iter()
        .map(|rs| {
            let resource = rs.resource.map(|r| OtelResource {
                attributes: r
                    .attributes
                    .into_iter()
                    .map(|kv| OtelKeyValue {
                        key: kv.key,
                        value: kv.value.map(|av| OtelAnyValue {
                            string_value: av.value.as_ref().and_then(|v| match v {
                                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => Some(s.clone()),
                                _ => None,
                            }),
                            int_value: av.value.as_ref().and_then(|v| match v {
                                opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => Some(i.to_string()),
                                _ => None,
                            }),
                            double_value: av.value.as_ref().and_then(|v| match v {
                                opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(f) => Some(*f),
                                _ => None,
                            }),
                            bool_value: av.value.as_ref().and_then(|v| match v {
                                opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => Some(*b),
                                _ => None,
                            }),
                            array_value: av.value.as_ref().and_then(|v| match v {
                                opentelemetry_proto::tonic::common::v1::any_value::Value::ArrayValue(arr) => Some(OtelArrayValue {
                                    values: arr
                                        .values
                                        .iter()
                                        .filter_map(|x| x.value.as_ref())
                                        .map(|vv| OtelAnyValue {
                                            string_value: match vv {
                                                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => Some(s.clone()),
                                                _ => None,
                                            },
                                            int_value: match vv {
                                                opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => Some(i.to_string()),
                                                _ => None,
                                            },
                                            double_value: match vv {
                                                opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(f) => Some(*f),
                                                _ => None,
                                            },
                                            bool_value: match vv {
                                                opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => Some(*b),
                                                _ => None,
                                            },
                                            array_value: None,
                                        })
                                        .collect(),
                                }),
                                _ => None,
                            }),
                        }),
                    })
                    .collect(),
            });

            let scope_spans = rs
                .scope_spans
                .into_iter()
                .map(|ss| OtelScopeSpan {
                    spans: ss
                        .spans
                        .into_iter()
                        .map(|s| OtelSpan {
                            trace_id: hex::encode(s.trace_id),
                            span_id: hex::encode(s.span_id),
                            parent_span_id: if s.parent_span_id.is_empty() {
                                None
                            } else {
                                Some(hex::encode(s.parent_span_id))
                            },
                            name: s.name,
                            start_time_unix_nano: if s.start_time_unix_nano == 0 {
                                None
                            } else {
                                Some(s.start_time_unix_nano.to_string())
                            },
                            end_time_unix_nano: if s.end_time_unix_nano == 0 {
                                None
                            } else {
                                Some(s.end_time_unix_nano.to_string())
                            },
                            attributes: s
                                .attributes
                                .into_iter()
                                .map(|kv| OtelKeyValue {
                                    key: kv.key,
                                    value: kv.value.map(|av| OtelAnyValue {
                                        string_value: av.value.as_ref().and_then(|v| match v {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => Some(s.clone()),
                                            _ => None,
                                        }),
                                        int_value: av.value.as_ref().and_then(|v| match v {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => Some(i.to_string()),
                                            _ => None,
                                        }),
                                        double_value: av.value.as_ref().and_then(|v| match v {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(f) => Some(*f),
                                            _ => None,
                                        }),
                                        bool_value: av.value.as_ref().and_then(|v| match v {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => Some(*b),
                                            _ => None,
                                        }),
                                        array_value: av.value.as_ref().and_then(|v| match v {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::ArrayValue(arr) => Some(OtelArrayValue {
                                                values: arr
                                                    .values
                                                    .iter()
                                                    .filter_map(|x| x.value.as_ref())
                                                    .map(|vv| OtelAnyValue {
                                                        string_value: match vv {
                                                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => Some(s.clone()),
                                                            _ => None,
                                                        },
                                                        int_value: match vv {
                                                            opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => Some(i.to_string()),
                                                            _ => None,
                                                        },
                                                        double_value: match vv {
                                                            opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(f) => Some(*f),
                                                            _ => None,
                                                        },
                                                        bool_value: match vv {
                                                            opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => Some(*b),
                                                            _ => None,
                                                        },
                                                        array_value: None,
                                                    })
                                                    .collect(),
                                            }),
                                            _ => None,
                                        }),
                                    }),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect();

            OtelResourceSpan {
                resource,
                scope_spans,
            }
        })
        .collect();

    OtelExportTraceServiceRequest { resource_spans }
}

pub(crate) async fn post_otel_traces(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let raw = ungzip_if_needed(&headers, body)?;
    let ct = content_type(&headers).unwrap_or_else(|| "application/json".to_string());

    let otel: OtelExportTraceServiceRequest = if ct == "application/json" {
        serde_json::from_slice(&raw)
            .map_err(|e| ApiError::BadRequest(format!("invalid json: {e}")))?
    } else if ct == "application/x-protobuf" {
        let pb = PbExportTraceServiceRequest::decode(raw.as_slice())
            .map_err(|e| ApiError::BadRequest(format!("invalid protobuf: {e}")))?;
        pb_to_otel_json(pb)
    } else {
        return Err(ApiError::BadRequest(format!(
            "unsupported content-type: {ct}"
        )));
    };

    let batches = map_otel_to_batches(&state, otel)?;
    for batch in batches {
        state.ingest_tx.try_send(batch).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ApiError::TooManyRequests,
            mpsc::error::TrySendError::Closed(_) => ApiError::ServiceUnavailable,
        })?;
    }

    Ok((StatusCode::OK, Json(serde_json::json!({}))))
}
