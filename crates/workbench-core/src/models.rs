use crate::{
    App,
    app::{id, now, text},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

pub fn save(app: &App, value: Value) -> Result<Value> {
    let connection = app.store.get("connection", text(&value, "connectionId"))?;
    if text(&value, "label").trim().is_empty() || text(&value, "modelId").trim().is_empty() {
        bail!("A profile needs a name and an exact model ID");
    }
    if value["temperature"]
        .as_f64()
        .is_some_and(|n| !(0.0..=2.0).contains(&n))
    {
        bail!("Temperature must be between 0 and 2");
    }
    if !value["reasoningEffort"].is_null()
        && !["", "none", "minimal", "low", "medium", "high", "xhigh"]
            .contains(&text(&value, "reasoningEffort"))
    {
        bail!("Unsupported reasoning effort");
    }
    if connection["authKind"] == "openai_oauth"
        && (value["temperature"].is_number() || !text(&value, "reasoningEffort").is_empty())
    {
        bail!(
            "This ChatGPT route uses its documented defaults; sampling/reasoning overrides are not enabled for it"
        );
    }
    for key in ["inputPrice", "outputPrice"] {
        if !value[key].is_null()
            && value[key]
                .as_f64()
                .is_none_or(|n| n < 0.0 || !n.is_finite())
        {
            bail!("Prices must be nonnegative USD per million tokens, or left unknown");
        }
    }
    let profile_id = if text(&value, "id").is_empty() {
        id()
    } else {
        text(&value, "id").to_owned()
    };
    let old = app.store.get("profile", &profile_id).unwrap_or(json!({}));
    let mut profile = json!({"id":profile_id,"label":text(&value,"label"),"connectionId":connection["id"],"modelId":text(&value,"modelId"),"contextWindow":value["contextWindow"].as_u64().unwrap_or(32768).clamp(512,10_000_000),"maxOutputTokens":value["maxOutputTokens"].as_u64().unwrap_or(4096).clamp(1,128000),"temperature":value["temperature"],"reasoningEffort":text(&value,"reasoningEffort"),"tools":value["tools"].as_bool().unwrap_or(false),"vision":value["vision"].as_bool().unwrap_or(false),"inputPrice":value["inputPrice"],"outputPrice":value["outputPrice"],"updatedAt":now(),"evidence":null});
    if old["connectionId"] == profile["connectionId"]
        && old["modelId"] == profile["modelId"]
        && old["route"] == connection["baseUrl"]
        && old["protocol"] == connection["protocol"]
        && old["authKind"] == connection["authKind"]
    {
        profile["evidence"] = old["evidence"].clone();
    }
    profile["route"] = connection["baseUrl"].clone();
    profile["protocol"] = connection["protocol"].clone();
    profile["authKind"] = connection["authKind"].clone();
    app.store.put("profile", &profile)?;
    Ok(profile)
}

pub fn effective(app: &App, chat: &Value) -> Result<Value> {
    let mut connection = app.store.get("connection", text(chat, "connectionId"))?;
    if !text(chat, "profileId").is_empty() {
        let profile = app.store.get("profile", text(chat, "profileId"))?;
        if profile["connectionId"] != chat["connectionId"] || profile["modelId"] != chat["modelId"]
        {
            bail!("The selected model profile does not match this task's model connection");
        }
        if profile["route"] != connection["baseUrl"]
            || profile["protocol"] != connection["protocol"]
            || profile["authKind"] != connection["authKind"]
        {
            bail!("The profile's endpoint changed; save/retest it before using it");
        }
        for key in [
            "contextWindow",
            "maxOutputTokens",
            "temperature",
            "reasoningEffort",
            "tools",
            "vision",
            "inputPrice",
            "outputPrice",
        ] {
            connection[key] = profile[key].clone();
        }
    }
    Ok(connection)
}

pub async fn probe(app: Arc<App>, profile_id: &str) -> Result<Value> {
    let mut profile = app.store.get("profile", profile_id)?;
    let chat = json!({"connectionId":profile["connectionId"],"modelId":profile["modelId"],"profileId":profile["id"]});
    let connection = effective(&app, &chat)?;
    let result = crate::providers::probe(&app, &connection, text(&profile, "modelId")).await?;
    profile["tools"] = result["tools"].clone();
    profile["evidence"] = json!({"at":now(),"route":connection["baseUrl"],"authKind":connection["authKind"],"text":result["text"],"streaming":result["streaming"],"tools":result["tools"],"vision":"manually configured; not tested by text/tool probe"});
    app.store.put("profile", &profile)?;
    Ok(json!({"profile":profile,"result":result}))
}

pub fn usage_tokens(usage: &Value) -> (u64, u64, bool) {
    let input = usage["input_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens"].as_u64())
        .or_else(|| usage["promptTokenCount"].as_u64());
    let output = usage["output_tokens"]
        .as_u64()
        .or_else(|| usage["completion_tokens"].as_u64())
        .or_else(|| {
            usage["candidatesTokenCount"]
                .as_u64()
                .map(|n| n + usage["thoughtsTokenCount"].as_u64().unwrap_or(0))
        });
    (
        input.unwrap_or(0),
        output.unwrap_or(0),
        input.is_some() && output.is_some(),
    )
}
#[allow(clippy::too_many_arguments)] // Explicit request identity, token and timing evidence at the collection boundary.
pub fn record_usage(
    app: &App,
    run: &str,
    chat: &str,
    connection: &Value,
    model: &str,
    usage: &Value,
    input_estimate: u64,
    output_estimate: u64,
    elapsed_ms: u64,
    first_token_ms: Option<u64>,
) -> Result<Value> {
    let (input, output, reported) = usage_tokens(usage);
    let (input, output) = if reported {
        (input, output)
    } else {
        (input.max(input_estimate), output.max(output_estimate))
    };
    let cost = connection["inputPrice"]
        .as_f64()
        .zip(connection["outputPrice"].as_f64())
        .map(|(a, b)| (input as f64 * a + output as f64 * b) / 1_000_000.0);
    let item = json!({"id":id(),"runId":run,"chatId":chat,"connectionId":connection["id"],"modelId":model,"inputTokens":input,"outputTokens":output,"providerReported":reported,"durationMs":elapsed_ms,"firstTokenMs":first_token_ms,"estimatedCostUsd":cost,"pricingSource":if cost.is_some(){"user configured rates; estimate"}else{"unknown"},"createdAt":now()});
    app.store.put("usage", &item)?;
    app.store.event(run, chat, "usage_recorded", item.clone())?;
    Ok(item)
}
pub fn check_budget(
    app: &App,
    chat: &Value,
    run: &str,
    connection: &Value,
    input_estimate: u64,
) -> Result<u64> {
    let used: Vec<Value> = app.store.usage_for_run(run)?;
    let max_requests = chat["budget"]["maxRequests"]
        .as_u64()
        .unwrap_or(50)
        .clamp(1, 1000);
    if used.len() as u64 >= max_requests {
        bail!("Task request budget reached");
    }
    let max_tokens = chat["budget"]["maxTokens"]
        .as_u64()
        .unwrap_or(1_000_000)
        .clamp(128, 100_000_000);
    let tokens: u64 = used
        .iter()
        .map(|u| u["inputTokens"].as_u64().unwrap_or(0) + u["outputTokens"].as_u64().unwrap_or(0))
        .sum();
    let remaining = max_tokens.saturating_sub(tokens + input_estimate);
    if remaining == 0 {
        bail!("Task token budget reached (input/context estimate included)");
    }
    let output = remaining.min(connection["maxOutputTokens"].as_u64().unwrap_or(4096));
    if let Some(ceiling) = chat["budget"]["maxCostUsd"].as_f64() {
        let (input_price,output_price)=connection["inputPrice"].as_f64().zip(connection["outputPrice"].as_f64()).context("A USD budget needs both token prices; unknown provider prices cannot enforce a cost estimate")?;
        let spent: f64 = used
            .iter()
            .filter_map(|u| u["estimatedCostUsd"].as_f64())
            .sum();
        if spent
            + (input_estimate as f64 * input_price + output as f64 * output_price) / 1_000_000.0
            > ceiling
        {
            bail!("Task cost estimate would exceed its configured budget");
        }
    }
    Ok(output)
}

pub fn compare(app: Arc<App>, value: Value) -> Result<Value> {
    let profiles = value["profileIds"]
        .as_array()
        .context("Select model profiles")?
        .clone();
    if profiles.is_empty() || profiles.len() > 8 || text(&value, "prompt").is_empty() {
        bail!("Choose 1–8 profiles and a comparison task");
    }
    for profile in &profiles {
        app.store
            .get("profile", profile.as_str().context("Invalid profile ID")?)?;
    }
    let comparison = json!({"id":id(),"prompt":value["prompt"],"profileIds":profiles,"expectedText":value["expectedText"],"status":"running","results":[],"createdAt":now()});
    app.store.put("comparison", &comparison)?;
    let result = comparison.clone();
    let token = CancellationToken::new();
    app.runs
        .lock()
        .insert(text(&comparison, "id").into(), token.clone());
    tokio::spawn(async move {
        let mut comparison = comparison;
        for profile_id in profiles {
            if token.is_cancelled() {
                comparison["status"] = json!("canceled");
                break;
            }
            let started = Instant::now();
            let mut first = None;
            let outcome=async{
                let profile=app.store.get("profile",profile_id.as_str().unwrap_or(""))?;
                let connection=effective(&app,&json!({"connectionId":profile["connectionId"],"modelId":profile["modelId"],"profileId":profile["id"]}))?;
                let generation=crate::providers::generate(&app,&connection,text(&profile,"modelId"),"Complete this comparison task. No tools or external side effects are available.",&[crate::providers::Message::new("user",text(&comparison,"prompt").into())],&[],&token,|_|{if first.is_none(){first=Some(started.elapsed().as_millis() as u64);}}).await?;
                let usage=record_usage(&app,text(&comparison,"id"),"",&connection,text(&profile,"modelId"),&generation.usage,text(&comparison,"prompt").len().div_ceil(4)as u64,generation.text.len().div_ceil(4)as u64,started.elapsed().as_millis()as u64,first)?;
                let tool_probe=if value["testTools"]==true{match crate::providers::probe_with_cancel(&app,&connection,text(&profile,"modelId"),&token).await{Ok(result)=>result,Err(error)=>json!({"error":error.to_string()})}}else{Value::Null};
                let expected=text(&comparison,"expectedText");
                Ok::<Value,anyhow::Error>(json!({"profileId":profile_id,"reply":generation.text,"usage":usage,"toolProbe":tool_probe,"passed":if expected.is_empty(){Value::Null}else{json!(generation.text.contains(expected))},"assertion":if expected.is_empty(){"not assessed"}else{"expected-text assertion"}}))
            }.await;
            comparison["results"].as_array_mut().unwrap().push(match outcome{Ok(v)=>v,Err(e)=>json!({"profileId":profile_id,"error":e.to_string(),"durationMs":started.elapsed().as_millis() as u64})});
            let _ = app.store.put("comparison", &comparison);
        }
        if comparison["status"] == "running" {
            comparison["status"] = json!("completed");
        }
        let _ = app.store.put("comparison", &comparison);
        app.runs.lock().remove(text(&comparison, "id"));
    });
    Ok(result)
}
