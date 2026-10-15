// ABOUTME: Legacy /api/cost-dashboard payload compatibility for the native host (P4).
// ABOUTME: Ports parseRangeParams/buildCostDashboardPayload semantics field-by-field from cost-dashboard-data.ts.

use chrono::{DateTime, Datelike, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::host_data::{parse_session_metrics, SessionMetrics};

/// Parsed-metrics cache for the cost scan. Session files are append-only, so
/// an unchanged `(mtime, len)` pair implies an unchanged parse result — range
/// switches reuse it instead of re-reading hundreds of MB of jsonl.
#[derive(Default)]
pub struct SessionMetricsCache {
    entries: HashMap<PathBuf, (std::time::SystemTime, u64, SessionMetrics)>,
}

impl SessionMetricsCache {
    fn get(&self, path: &Path, mtime: std::time::SystemTime, len: u64) -> Option<&SessionMetrics> {
        self.entries
            .get(path)
            .and_then(|(m, l, metrics)| (*m == mtime && *l == len).then_some(metrics))
    }

    fn put(
        &mut self,
        path: PathBuf,
        mtime: std::time::SystemTime,
        len: u64,
        metrics: SessionMetrics,
    ) {
        self.entries.insert(path, (mtime, len, metrics));
    }
}

/// Query parameters of the legacy `/api/cost-dashboard` surface
/// (`parseRangeParams`): range, granularity, scope=all|current, models CSV,
/// plus custom from/to date-only bounds.
#[derive(Debug, Clone)]
pub struct CostRangeParams {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub range: String,
    pub granularity: String,
    pub scope: String,
    pub models: HashSet<String>,
}

fn query_value<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn js_number(value: f64) -> f64 {
    // `Number(value || 0)`: NaN and zero collapse to zero.
    if value != 0.0 {
        value
    } else {
        0.0
    }
}

/// Serialize a float the way JavaScript JSON.stringify does: integral
/// values print without a fractional part (1.0 -> 1).
fn js_value_number(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

/// Port of `parseDateOnly`/`new Date`: RFC3339 first, then date-only strings
/// which JavaScript parses as UTC midnight.
pub fn parse_js_date(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(parsed.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// Port of `parseRangeParams`.
pub fn parse_cost_range_params(query: &[(String, String)]) -> Option<CostRangeParams> {
    let range = query_value(query, "range")
        .filter(|value| !value.is_empty())
        .unwrap_or("30d")
        .to_ascii_lowercase();
    let granularity = query_value(query, "granularity")
        .filter(|value| !value.is_empty())
        .unwrap_or("day")
        .to_ascii_lowercase();
    let scope = query_value(query, "scope")
        .filter(|value| !value.is_empty())
        .unwrap_or("all")
        .to_ascii_lowercase();
    let models: HashSet<String> = query_value(query, "models")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    let now = Utc::now();
    let mut from = now - chrono::Duration::days(30);
    let mut to = now;
    if range == "7d" {
        from = now - chrono::Duration::days(7);
    } else if range == "90d" {
        from = now - chrono::Duration::days(90);
    } else if range == "custom" {
        if let Some(value) = query_value(query, "from").and_then(parse_js_date) {
            from = value;
        }
        if let Some(value) = query_value(query, "to").and_then(parse_js_date) {
            to = value;
        }
    }
    if to < from {
        std::mem::swap(&mut from, &mut to);
    }

    Some(CostRangeParams {
        from,
        to,
        range,
        granularity: if granularity == "week" || granularity == "month" {
            granularity
        } else {
            "day".to_string()
        },
        scope: if scope == "all" { "all" } else { "current" }.to_string(),
        models,
    })
}

fn day_key(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Port of `bucketForDate`: UTC day / ISO week / month bucket keys.
fn bucket_for_date(date: DateTime<Utc>, granularity: &str) -> String {
    if granularity == "month" {
        return date.format("%Y-%m").to_string();
    }
    if granularity == "day" {
        return date.format("%Y-%m-%d").to_string();
    }
    // ISO 8601 week numbering — identical to the JS manual algorithm.
    format!("{}-W{:02}", date.iso_week().year(), date.iso_week().week())
}

fn project_name(workspace: &str) -> String {
    let trimmed = workspace.trim();
    if trimmed.is_empty() {
        return "Unknown Project".to_string();
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|base| base.to_str())
        .map(str::to_string)
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| trimmed.to_string())
}

/// The legacy `CostSession` record pushed by `serveCostDashboard`.
#[derive(Debug, Clone)]
pub struct LegacyCostSession {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub model: String,
    pub time: DateTime<Utc>,
    pub total_cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub tool_calls: u64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub cost_per_user_message: f64,
    pub tool_cost_by_name: BTreeMap<String, f64>,
}

/// Apply the `serveCostDashboard` admission filters to parsed session
/// metrics: scope=current restricts sessions to the current workspace root,
/// the models allowlist restricts by model, and the session time
/// (lastActive, falling back to timestamp) must fall inside the window.
pub fn legacy_cost_session(
    metrics: &SessionMetrics,
    current_root: &Path,
    params: &CostRangeParams,
) -> Option<LegacyCostSession> {
    if params.scope == "current" {
        let matches_root = metrics
            .cwd_canonical
            .as_deref()
            .is_some_and(|cwd| cwd == current_root);
        if !matches_root {
            return None;
        }
    }
    if !params.models.is_empty() && !params.models.contains(&metrics.model) {
        return None;
    }
    let time = metrics.last_active.or_else(|| {
        (!metrics.timestamp.is_empty())
            .then(|| parse_js_date(&metrics.timestamp))
            .flatten()
    });
    let time = time?;
    if time < params.from || time > params.to {
        return None;
    }
    let workspace = metrics
        .cwd
        .as_ref()
        .map(|cwd| cwd.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(LegacyCostSession {
        id: metrics.id.clone(),
        title: if metrics.title.is_empty() {
            "Untitled".to_string()
        } else {
            metrics.title.clone()
        },
        workspace,
        model: if metrics.model.is_empty() {
            "unknown".to_string()
        } else {
            metrics.model.clone()
        },
        time,
        total_cost: metrics.total_cost,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cache_read: metrics.cache_read,
        cache_write: metrics.cache_write,
        total_tokens: metrics.input_tokens + metrics.output_tokens + metrics.cache_read,
        tool_calls: metrics.tool_calls,
        user_messages: metrics.user_messages,
        assistant_messages: metrics.assistant_messages,
        cost_per_user_message: if metrics.user_messages > 0 {
            metrics.total_cost / metrics.user_messages as f64
        } else {
            metrics.total_cost
        },
        tool_cost_by_name: metrics
            .tool_cost_by_name
            .iter()
            .map(|(name, cost)| (name.clone(), *cost))
            .collect(),
    })
}

pub(crate) fn empty_payload(params: &CostRangeParams) -> Value {
    json!({
        "range": {
            "from": params.from.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "to": params.to.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "granularity": params.granularity,
            "scope": params.scope,
            "range": params.range,
        },
        "summary": {
            "totalCost": 0,
            "totalTokens": 0,
            "sessionCount": 0,
            "userMessageCount": 0,
            "avgCostPerSession": 0,
            "avgCostPerUserMessage": 0,
        },
        "series": [],
        "breakdown": { "byModel": [], "byTool": [] },
        "topSessions": [],
        "sessions": [],
        "infobar": {
            "overview": {
                "totalCost": 0,
                "sessionCount": 0,
                "messageCount": 0,
                "daysActive": 0,
                "avgCostPerDay": 0,
                "todayCost": 0,
            },
            "models": [],
            "projects": [],
            "usage": {
                "totalTokens": 0,
                "inputTokens": 0,
                "outputTokens": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "toolCalls": 0,
                "tools": [],
            },
        },
    })
}

fn session_json(session: &LegacyCostSession) -> Value {
    json!({
        "id": session.id,
        "title": session.title,
        "workspace": session.workspace,
        "model": session.model,
        "time": session.time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "totalCost": js_value_number(session.total_cost),
        "inputTokens": session.input_tokens,
        "outputTokens": session.output_tokens,
        "cacheRead": session.cache_read,
        "cacheWrite": session.cache_write,
        "totalTokens": session.total_tokens,
        "toolCalls": session.tool_calls,
        "userMessages": session.user_messages,
        "assistantMessages": session.assistant_messages,
        "costPerUserMessage": js_value_number(session.cost_per_user_message),
        "toolCostByName": session.tool_cost_by_name,
    })
}

/// Port of `buildCostDashboardPayload`: aggregation, buckets, sorting and
/// infobar fractions over the admitted sessions, in the legacy response
/// shape. Session order and accumulation order match the TS source so float
/// bits (and therefore JSON) match on identical fixtures.
pub fn build_cost_payload(
    sessions: Vec<LegacyCostSession>,
    params: &CostRangeParams,
    now: DateTime<Utc>,
) -> Value {
    let mut payload = empty_payload(params)
        .as_object_mut()
        .expect("payload object")
        .clone();

    let mut sorted = sessions;
    sorted.sort_by_key(|session| std::cmp::Reverse(session.time));
    payload["sessions"] = Value::Array(sorted.iter().map(session_json).collect());
    payload["summary"]["sessionCount"] = Value::from(sorted.len() as u64);

    // Vec with find-or-insert mirrors JS Map insertion semantics so stable
    // cost sorts resolve ties identically.
    let mut by_model: Vec<(String, f64, u64)> = Vec::new();
    let mut by_tool: Vec<(String, f64)> = Vec::new();
    let mut by_bucket: Vec<(String, f64, u64)> = Vec::new();
    let mut by_project: Vec<(String, String, String, f64, u64)> = Vec::new();
    let mut infobar_tools: Vec<(String, f64, u64)> = Vec::new();
    let mut active_days: HashSet<String> = HashSet::new();
    let today_key = day_key(now);

    for session in &sorted {
        let total_cost = js_number(session.total_cost);
        let total_tokens = session.total_tokens;
        payload["summary"]["totalCost"] = json!(js_value_number(
            payload["summary"]["totalCost"].as_f64().unwrap_or(0.0) + total_cost
        ));
        payload["summary"]["totalTokens"] =
            Value::from(payload["summary"]["totalTokens"].as_u64().unwrap_or(0) + total_tokens);
        payload["summary"]["userMessageCount"] = Value::from(
            payload["summary"]["userMessageCount"].as_u64().unwrap_or(0) + session.user_messages,
        );

        let model_key = if session.model.is_empty() {
            "unknown"
        } else {
            &session.model
        };
        match by_model.iter_mut().find(|(name, _, _)| name == model_key) {
            Some((_, cost, count)) => {
                *cost += total_cost;
                *count += 1;
            }
            None => by_model.push((model_key.to_string(), total_cost, 1)),
        };

        let bucket = bucket_for_date(session.time, &params.granularity);
        match by_bucket.iter_mut().find(|(key, _, _)| *key == bucket) {
            Some((_, cost, tokens)) => {
                *cost += total_cost;
                *tokens += total_tokens;
            }
            None => by_bucket.push((bucket, total_cost, total_tokens)),
        }

        let session_day = day_key(session.time);
        active_days.insert(session_day.clone());
        if session_day == today_key {
            let today = payload["infobar"]["overview"]["todayCost"]
                .as_f64()
                .unwrap_or(0.0);
            payload["infobar"]["overview"]["todayCost"] =
                json!(js_value_number(today + total_cost));
        }

        let project_key = if session.workspace.is_empty() {
            "unknown-project"
        } else {
            &session.workspace
        };
        match by_project.iter_mut().find(|(key, ..)| key == project_key) {
            Some((_, _, _, cost, sessions_count)) => {
                *cost += total_cost;
                *sessions_count += 1;
            }
            None => by_project.push((
                project_key.to_string(),
                project_name(&session.workspace),
                session.workspace.clone(),
                total_cost,
                1,
            )),
        }

        for (tool_name, tool_cost) in &session.tool_cost_by_name {
            let tool_cost = js_number(*tool_cost);
            match by_tool.iter_mut().find(|(name, _)| name == tool_name) {
                Some((_, cost)) => *cost += tool_cost,
                None => by_tool.push((tool_name.clone(), tool_cost)),
            }
            match infobar_tools
                .iter_mut()
                .find(|(name, _, _)| name == tool_name)
            {
                Some((_, cost, count)) => {
                    *cost += tool_cost;
                    *count += 1;
                }
                None => infobar_tools.push((tool_name.clone(), tool_cost, 1)),
            }
            let usage_tool = match payload["infobar"]["usage"]["tools"]
                .as_array()
                .expect("usage tools array")
                .iter()
                .position(|tool| tool["name"] == *tool_name)
            {
                Some(index) => index,
                None => {
                    payload["infobar"]["usage"]["tools"]
                        .as_array_mut()
                        .expect("usage tools array")
                        .push(
                            json!({ "name": tool_name, "cost": 0.0, "count": 0, "fraction": 0.0 }),
                        );
                    payload["infobar"]["usage"]["tools"]
                        .as_array()
                        .expect("usage tools array")
                        .len()
                        - 1
                }
            };
            let tool = &mut payload["infobar"]["usage"]["tools"][usage_tool];
            tool["cost"] = json!(js_value_number(
                tool["cost"].as_f64().unwrap_or(0.0) + tool_cost
            ));
            tool["count"] = Value::from(tool["count"].as_u64().unwrap_or(0) + 1);
        }

        let overview = &mut payload["infobar"]["overview"];
        overview["totalCost"] = json!(js_value_number(
            overview["totalCost"].as_f64().unwrap_or(0.0) + total_cost
        ));
        overview["sessionCount"] = Value::from(overview["sessionCount"].as_u64().unwrap_or(0) + 1);
        overview["messageCount"] = Value::from(
            overview["messageCount"].as_u64().unwrap_or(0)
                + session.user_messages
                + session.assistant_messages,
        );
        let usage = &mut payload["infobar"]["usage"];
        usage["totalTokens"] =
            Value::from(usage["totalTokens"].as_u64().unwrap_or(0) + total_tokens);
        usage["inputTokens"] =
            Value::from(usage["inputTokens"].as_u64().unwrap_or(0) + session.input_tokens);
        usage["outputTokens"] =
            Value::from(usage["outputTokens"].as_u64().unwrap_or(0) + session.output_tokens);
        usage["cacheRead"] =
            Value::from(usage["cacheRead"].as_u64().unwrap_or(0) + session.cache_read);
        usage["cacheWrite"] =
            Value::from(usage["cacheWrite"].as_u64().unwrap_or(0) + session.cache_write);
        usage["toolCalls"] =
            Value::from(usage["toolCalls"].as_u64().unwrap_or(0) + session.tool_calls);
    }

    payload["summary"]["avgCostPerSession"] = json!(js_value_number(
        if payload["summary"]["sessionCount"].as_u64().unwrap_or(0) > 0 {
            payload["summary"]["totalCost"].as_f64().unwrap_or(0.0)
                / payload["summary"]["sessionCount"].as_u64().unwrap_or(0) as f64
        } else {
            0.0
        }
    ));
    payload["summary"]["avgCostPerUserMessage"] = json!(js_value_number(
        if payload["summary"]["userMessageCount"].as_u64().unwrap_or(0) > 0 {
            payload["summary"]["totalCost"].as_f64().unwrap_or(0.0)
                / payload["summary"]["userMessageCount"].as_u64().unwrap_or(0) as f64
        } else {
            0.0
        }
    ));

    by_bucket.sort_by(|a, b| a.0.cmp(&b.0));
    payload["series"] = Value::Array(
        by_bucket
            .iter()
            .map(|(bucket, cost, tokens)| {
                json!({ "bucket": bucket, "cost": js_value_number(*cost), "tokens": tokens })
            })
            .collect(),
    );

    by_model.sort_by(|a, b| b.1.total_cmp(&a.1));
    payload["breakdown"]["byModel"] = Value::Array(
        by_model
            .iter()
            .map(|(name, cost, _)| json!({ "name": name, "cost": js_value_number(*cost) }))
            .collect(),
    );
    by_tool.sort_by(|a, b| b.1.total_cmp(&a.1));
    payload["breakdown"]["byTool"] = Value::Array(
        by_tool
            .iter()
            .map(|(name, cost)| json!({ "name": name, "cost": js_value_number(*cost) }))
            .collect(),
    );

    let mut top = sorted.clone();
    top.sort_by(|a, b| b.total_cost.total_cmp(&a.total_cost));
    top.truncate(20);
    payload["topSessions"] = Value::Array(top.iter().map(session_json).collect());

    payload["infobar"]["overview"]["daysActive"] = Value::from(active_days.len() as u64);
    payload["infobar"]["overview"]["avgCostPerDay"] =
        json!(js_value_number(if active_days.is_empty() {
            0.0
        } else {
            payload["infobar"]["overview"]["totalCost"]
                .as_f64()
                .unwrap_or(0.0)
                / active_days.len() as f64
        }));

    by_model.sort_by(|a, b| b.1.total_cmp(&a.1));
    let max_model_cost = by_model.first().map(|(_, cost, _)| *cost).unwrap_or(0.0);
    payload["infobar"]["models"] = Value::Array(
        by_model
            .iter()
            .map(|(name, cost, count)| {
                json!({
                    "name": name,
                    "cost": js_value_number(*cost),
                    "count": count,
                    "fraction": js_value_number(if max_model_cost > 0.0 { cost / max_model_cost } else { 0.0 }),
                })
            })
            .collect(),
    );

    by_project.sort_by(|a, b| b.3.total_cmp(&a.3));
    let max_project_cost = by_project
        .first()
        .map(|(_, _, _, cost, _)| *cost)
        .unwrap_or(0.0);
    payload["infobar"]["projects"] = Value::Array(
        by_project
            .iter()
            .map(|(_key, name, path, cost, sessions)| {
                json!({
                    "name": name,
                    "path": path,
                    "cost": js_value_number(*cost),
                    "sessions": sessions,
                    "fraction": js_value_number(if max_project_cost > 0.0 { cost / max_project_cost } else { 0.0 }),
                })
            })
            .collect(),
    );

    infobar_tools.sort_by(|a, b| b.1.total_cmp(&a.1));
    let max_tool_cost = infobar_tools
        .first()
        .map(|(_, cost, _)| *cost)
        .unwrap_or(0.0);
    payload["infobar"]["usage"]["tools"] = Value::Array(
        infobar_tools
            .iter()
            .map(|(name, cost, count)| {
                json!({
                    "name": name,
                    "cost": js_value_number(*cost),
                    "count": count,
                    "fraction": js_value_number(if max_tool_cost > 0.0 { cost / max_tool_cost } else { 0.0 }),
                })
            })
            .collect(),
    );

    Value::Object(payload)
}

/// Scan the shared session tree and build the legacy cost payload for the
/// compat endpoint. `current_root` is the workspace root that "current"
/// scope compares against (the queried workspace).
pub fn scan_compat_cost_dashboard(
    session_root: &Path,
    current_root: &Path,
    params: &CostRangeParams,
    now: DateTime<Utc>,
    cache: Option<&Mutex<SessionMetricsCache>>,
) -> Result<Value, String> {
    if !session_root.is_dir() {
        return Ok(empty_payload(params));
    }
    // Collect candidates first with a cheap mtime pre-filter: a session file
    // is append-only, so its last modification bounds the newest entry —
    // files untouched since before `from` cannot contribute to this range
    // and are skipped without reading (298MB of history scans as metadata).
    // Files carry their (mtime, len) so the parse cache can validate entries.
    let mut candidates = Vec::new();
    for project in std::fs::read_dir(session_root)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
    {
        if !project.path().is_dir() {
            continue;
        }
        for file in std::fs::read_dir(project.path())
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
        {
            let path = file.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let stat = std::fs::metadata(&path)
                .ok()
                .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
            match stat {
                // Entries can never be newer than the file's last write.
                Some((modified, _)) if DateTime::<Utc>::from(modified) < params.from => {
                    continue;
                }
                Some((modified, len)) => candidates.push((path, modified, len)),
                None => candidates.push((path, std::time::SystemTime::UNIX_EPOCH, 0)),
            }
        }
    }
    // Biggest file first: a handful of multi-MB sessions dominates the scan,
    // so size-ordering keeps the fixed-count chunks byte-balanced across
    // workers instead of letting the giants land in one chunk.
    candidates.sort_by_key(|&(_, _, len)| std::cmp::Reverse(len));

    // Cache hits resolve without touching the file; only misses reach the
    // parallel parse below. The lock is held just for this split and the
    // insert afterwards — never across worker threads.
    let mut metrics_all = Vec::new();
    let mut misses = Vec::new();
    if let Some(cache) = cache {
        let guard = cache.lock().map_err(|_| "cost cache poisoned".to_owned())?;
        for (path, modified, len) in &candidates {
            match guard.get(path, *modified, *len) {
                Some(metrics) => metrics_all.push(metrics.clone()),
                None => misses.push((path.clone(), *modified, *len)),
            }
        }
    } else {
        misses = candidates;
    }

    // Parse misses in parallel on plain std threads (no async runtime here,
    // no new deps): JSON line parsing is CPU-bound and dominates the cold
    // scan. build_cost_payload sorts every aggregate deterministically, so
    // thread merge order cannot change the payload.
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .clamp(1, 8);
    let chunk_size = misses.len().div_ceil(workers).max(1);
    let mut parsed: Vec<(PathBuf, std::time::SystemTime, u64, SessionMetrics)> = Vec::new();
    std::thread::scope(|scope| -> Result<(), String> {
        let handles: Vec<_> = misses
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut chunk_metrics = Vec::with_capacity(chunk.len());
                    for (path, modified, len) in chunk {
                        let metrics = parse_session_metrics(path)
                            .map_err(|_| "session parse failed".to_owned())?;
                        if let Some(metrics) = metrics {
                            chunk_metrics.push((path.clone(), *modified, *len, metrics));
                        }
                    }
                    Ok(chunk_metrics)
                })
            })
            .collect();
        for handle in handles {
            let chunk_metrics = handle
                .join()
                .map_err(|_| "session parse failed".to_owned())
                .and_then(|result| result)?;
            parsed.extend(chunk_metrics);
        }
        Ok(())
    })?;
    if let Some(cache) = cache {
        let mut guard = cache.lock().map_err(|_| "cost cache poisoned".to_owned())?;
        for (path, modified, len, metrics) in &parsed {
            guard.put(path.clone(), *modified, *len, metrics.clone());
        }
    }
    metrics_all.extend(parsed.into_iter().map(|(_, _, _, metrics)| metrics));

    let sessions = metrics_all
        .iter()
        .filter_map(|metrics| legacy_cost_session(metrics, current_root, params))
        .collect();
    Ok(build_cost_payload(sessions, params, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_payload_matches_legacy_ts_on_identical_fixtures() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("picot-cost-parity-{nonce}"));
        let workspace = root.join("workspace");
        let project_a = root.join("sessions").join("--workspace-proj--");
        let project_b = root.join("sessions").join("--other-proj--");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        let cwd_a = serde_json::to_string(&workspace.to_string_lossy()).unwrap();
        let cwd_b = serde_json::to_string(&root.join("other").to_string_lossy()).unwrap();
        std::fs::write(
            project_a.join("a1.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"a1\",\"cwd\":{cwd_a}}}\n{{\"type\":\"session_info\",\"name\":\"Alpha\"}}\n{{\"type\":\"model_change\",\"model\":\"model-x\"}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"user\"}},\"timestamp\":\"2026-08-10T09:00:00.000Z\"}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"model-x\",\"usage\":{{\"cost\":{{\"total\":3.5}},\"input\":100,\"output\":50,\"cacheRead\":10,\"cacheWrite\":5}},\"content\":[{{\"type\":\"toolCall\",\"name\":\"bash\"}},{{\"type\":\"toolCall\",\"name\":\"read\"}}]}},\"timestamp\":\"2026-08-10T09:05:00.000Z\"}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            project_a.join("a2.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"a2\",\"cwd\":{cwd_a}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"model-y\",\"usage\":{{\"cost\":{{\"total\":1.25}},\"input\":40,\"output\":20}}}},\"content\":[{{\"type\":\"toolCall\",\"name\":\"bash\"}}]}},\"timestamp\":\"2026-08-11T08:00:00.000Z\"}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            project_b.join("b1.jsonl"),
            format!(
                "{{\"type\":\"session\",\"id\":\"b1\",\"cwd\":{cwd_b}}}\n{{\"type\":\"message\",\"message\":{{\"role\":\"assistant\",\"model\":\"model-x\",\"usage\":{{\"cost\":{{\"total\":99.0}}}}}}}}\n"
            ),
        )
        .unwrap();

        let params = CostRangeParams {
            from: parse_js_date("2026-08-01T00:00:00Z").unwrap(),
            to: parse_js_date("2026-09-01T00:00:00Z").unwrap(),
            range: "custom".into(),
            granularity: "day".into(),
            scope: "all".into(),
            models: HashSet::new(),
        };
        let now = parse_js_date("2026-08-30T12:00:00Z").unwrap();
        let cache = std::sync::Mutex::new(SessionMetricsCache::default());
        let rust_payload = scan_compat_cost_dashboard(
            &root.join("sessions"),
            &workspace,
            &params,
            now,
            Some(&cache),
        )
        .unwrap();

        let params_json = serde_json::json!({
            "range": "custom",
            "from": "2026-08-01T00:00:00.000Z",
            "to": "2026-09-01T00:00:00.000Z",
            "granularity": "day",
            "scope": "all",
            "models": [],
        })
        .to_string();
        let script = std::path::PathBuf::from(env!("PICOT_SRC_TAURI_DIR"))
            .join("..")
            .join("scripts")
            .join("p4-cost-parity.mjs");
        let output = std::process::Command::new("bun")
            .arg(&script)
            .arg(root.join("sessions"))
            .arg(&workspace)
            .arg(&params_json)
            .arg("2026-08-30T12:00:00Z")
            .output()
            .expect("bun harness must run");
        assert!(
            output.status.success(),
            "TS harness failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let ts_payload: Value = serde_json::from_slice(&output.stdout).expect("TS payload JSON");
        assert_eq!(
            rust_payload, ts_payload,
            "Rust and legacy TS payloads must match field-by-field"
        );

        // scope=current keeps only the workspace-scoped project.
        let current_params = CostRangeParams {
            scope: "current".into(),
            ..params
        };
        let current_payload = scan_compat_cost_dashboard(
            &root.join("sessions"),
            &workspace,
            &current_params,
            now,
            Some(&cache),
        )
        .unwrap();
        let session_rows = current_payload["sessions"].as_array().unwrap();
        assert!(
            session_rows
                .iter()
                .all(|row| row["workspace"] == workspace.to_string_lossy().as_ref()),
            "scope=current must exclude other-project sessions"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn session(model: &str, time: &str, cost: f64) -> LegacyCostSession {
        LegacyCostSession {
            id: format!("id-{model}-{time}"),
            title: "T".into(),
            workspace: "/w".into(),
            model: model.into(),
            time: parse_js_date(time).unwrap(),
            total_cost: cost,
            input_tokens: 10,
            output_tokens: 20,
            cache_read: 5,
            cache_write: 0,
            total_tokens: 35,
            tool_calls: 2,
            user_messages: 3,
            assistant_messages: 4,
            cost_per_user_message: if cost != 0.0 { cost / 3.0 } else { cost },
            tool_cost_by_name: BTreeMap::from([("bash".into(), cost / 2.0)]),
        }
    }

    #[test]
    fn parse_cost_params_defaults_and_normalization() {
        let params = parse_cost_range_params(&[]).unwrap();
        assert_eq!(params.range, "30d");
        assert_eq!(params.granularity, "day");
        assert_eq!(params.scope, "all");
        assert!(params.models.is_empty());

        let params = parse_cost_range_params(&[
            ("range".into(), "CUSTOM".into()),
            ("from".into(), "2026-08-01".into()),
            ("to".into(), "2026-09-01".into()),
            ("granularity".into(), "week".into()),
            ("scope".into(), "current".into()),
            ("models".into(), " a , ,b ".into()),
        ])
        .unwrap();
        assert_eq!(params.range, "custom");
        assert_eq!(params.from.format("%Y-%m-%d").to_string(), "2026-08-01");
        assert_eq!(params.granularity, "week");
        assert_eq!(params.scope, "current");
        assert_eq!(
            params.models,
            HashSet::from(["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn iso_week_bucket_matches_iso8601_numbering() {
        // 2027-01-02 is a Saturday belonging to ISO week 2026-W53.
        let saturday = parse_js_date("2027-01-02T00:00:00Z").unwrap();
        assert_eq!(bucket_for_date(saturday, "week"), "2026-W53");
        // 2026-01-01 (Thursday) opens ISO week 2026-W01.
        let thursday = parse_js_date("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(bucket_for_date(thursday, "week"), "2026-W01");
        assert_eq!(bucket_for_date(thursday, "month"), "2026-01");
        assert_eq!(bucket_for_date(thursday, "day"), "2026-01-01");
    }

    #[test]
    fn payload_tracks_summary_series_and_model_fractions() {
        let params = CostRangeParams {
            from: parse_js_date("2026-08-01T00:00:00Z").unwrap(),
            to: parse_js_date("2026-09-01T00:00:00Z").unwrap(),
            range: "custom".into(),
            granularity: "day".into(),
            scope: "all".into(),
            models: HashSet::new(),
        };
        let sessions = vec![
            session("model-a", "2026-08-10T10:00:00Z", 30.0),
            session("model-b", "2026-08-11T10:00:00Z", 10.0),
            session("model-a", "2026-08-12T10:00:00Z", 20.0),
        ];
        let now = parse_js_date("2026-08-12T23:00:00Z").unwrap();
        let payload = build_cost_payload(sessions, &params, now);

        assert_eq!(payload["summary"]["sessionCount"], 3);
        assert_eq!(payload["summary"]["totalTokens"], 105);
        assert_eq!(payload["breakdown"]["byModel"].as_array().unwrap().len(), 2);
        assert_eq!(payload["breakdown"]["byModel"][0]["name"], "model-a");
        // Sessions are sorted newest first.
        assert_eq!(payload["sessions"][0]["model"], "model-a");
        assert_eq!(payload["sessions"][0]["time"], "2026-08-12T10:00:00.000Z");
        // todayCost counts only sessions on the `now` day key.
        assert_eq!(payload["infobar"]["overview"]["todayCost"], 20.0);
        assert_eq!(payload["infobar"]["overview"]["daysActive"], 3);
        // topSessions is capped at 20 and ordered by cost.
        assert_eq!(payload["topSessions"][0]["totalCost"], 30.0);
        // Model fraction: max cost model has fraction 1.
        assert_eq!(payload["infobar"]["models"][0]["fraction"], 1.0);
        assert_eq!(
            payload["infobar"]["models"][1]["fraction"],
            json!(10.0 / 50.0)
        );
        // Series buckets are sorted ascending by key.
        let series = payload["series"].as_array().unwrap();
        assert_eq!(series.len(), 3);
        assert!(series.windows(2).all(|pair| {
            let (left, right) = (pair[0]["bucket"].as_str(), pair[1]["bucket"].as_str());
            left.unwrap_or("") <= right.unwrap_or("")
        }));
    }
}
