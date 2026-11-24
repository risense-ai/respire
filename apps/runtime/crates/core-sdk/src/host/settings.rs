use anyhow::{Context, Result};
use serde_json::{json, Value};

fn explicit_setting<T: std::str::FromStr + serde::Serialize>(
    settings: &mut serde_json::Map<String, Value>,
    key: &str,
    name: &str,
) -> Result<()> {
    match crate::env::var(name) {
        Ok(value) => {
            let value: T = value
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid {name}"))?;
            let value = serde_json::to_value(value)?;
            anyhow::ensure!(!value.is_null(), "invalid {name}");
            settings.insert(key.to_owned(), value);
            Ok(())
        }
        Err(std::env::VarError::NotPresent) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn request_settings(payload: &mut Value) -> Result<()> {
    crate::feedback_contract::validate(payload, Some(crate::FEEDBACK_CONTRACT))?;
    if let Some(fields) = payload.as_object_mut() {
        if !fields.contains_key("query_settings") {
            let settings = query_settings_from_env()?;
            fields.insert("query_settings".to_owned(), Value::Object(settings));
        }
        let settings = fields
            .get_mut("query_settings")
            .and_then(Value::as_object_mut)
            .context("query_settings must be an object")?;
        if !settings.contains_key("now") {
            settings.insert(
                "now".into(),
                json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            );
        }
        if !fields.contains_key("related_settings") {
            let mut settings = serde_json::Map::new();
            match crate::env::var("RSRS_RELATED") {
                Ok(value) => {
                    settings.insert("enabled".to_owned(), json!(value != "0"));
                }
                Err(std::env::VarError::NotPresent) => {}
                Err(error) => return Err(error.into()),
            }
            explicit_setting::<usize>(&mut settings, "max", "RSRS_RELATED_MAX")?;
            explicit_setting::<u64>(&mut settings, "pair_min", "RSRS_RELATED_PAIR_MIN")?;
            explicit_setting::<f32>(&mut settings, "min_cos", "RSRS_RELATED_MIN_COS")?;
            explicit_setting::<usize>(&mut settings, "knn_seeds", "RSRS_RELATED_KNN_SEEDS")?;
            explicit_setting::<usize>(&mut settings, "knn_per", "RSRS_RELATED_KNN_PER")?;
            fields.insert("related_settings".to_owned(), Value::Object(settings));
        }
    }
    Ok(())
}

/// Strict host opt-in; compatibility environment aliases are handled by env::var.
pub fn feedback_enabled() -> Result<bool> {
    feedback_switch("RSRS_FEEDBACK", false)
}
fn feedback_switch(name: &str, default: bool) -> Result<bool> {
    match crate::env::var(name) {
        Ok(value) => match value.trim() {
            "1" => Ok(true),
            "0" => Ok(false),
            _ => anyhow::bail!("{name} must be 0 or 1"),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

/// Host environment defaults, copied into each immutable request.
pub fn query_settings_from_env() -> Result<serde_json::Map<String, Value>> {
    let mut settings = serde_json::Map::new();
    match crate::env::var("RSRS_RECALL_PROFILE") {
        Ok(value) if value == "legacy" => {
            settings = serde_json::from_value(json!({"cos_min":0.0,"sim_min":0.30,
                "recall_min_score":0.25,"lexical_weight":0.30,"semantic_power":1.3,
                "bm25":false,"max_fusion":false,"normalize_ablation":false,
                "aux_cap_ratio":null,"rrf_cap":0.0,"heat_cap":15,"fusion":"legacy"}))?;
        }
        Ok(value) if value == "calibrated" => {}
        Ok(_) => anyhow::bail!("RSRS_RECALL_PROFILE must be calibrated or legacy"),
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(error.into()),
    }
    if feedback_enabled()? {
        anyhow::ensure!(crate::FEEDBACK_CONTRACT==1,"enabled feedback requires Core SDK feedback contract 1; install the paired SDK/CLI version");
        // Non-recall ranking callers still remove read heat. Recall paths
        // explicitly replace these neutral counts with their authorized context.
        settings.insert("feedback".into(),json!({"enabled":true,"good":{},"bad":{},"bad_penalty":feedback_switch("RSRS_BAD_PENALTY",true)?}));
    }
    match crate::env::var("RSRS_ROUTE") {
        Ok(value) => {
            settings.insert("route_on".to_owned(), json!(value != "0"));
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(error.into()),
    }
    match crate::env::var("RSRS_ABLATE") {
        Ok(value) => {
            settings.insert("ablate".to_owned(), json!(value));
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(error.into()),
    }
    for (key, name) in [
        ("cos_min", "RSRS_RECALL_COS_MIN"),
        ("sim_min", "RSRS_RECALL_SIM_MIN"),
        ("cos_relative_ratio", "RSRS_RECALL_COS_RELATIVE_RATIO"),
        ("cos_margin", "RSRS_RECALL_COS_MARGIN"),
        ("lexical_weight", "RSRS_RECALL_LEXICAL_WEIGHT"),
        ("semantic_power", "RSRS_RECALL_SEMANTIC_POWER"),
        ("aux_cap_ratio", "RSRS_RECALL_AUX_CAP_RATIO"),
        ("rrf_cap", "RSRS_RECALL_RRF_CAP"),
    ] {
        explicit_setting::<f32>(&mut settings, key, name)?;
    }
    explicit_setting::<String>(&mut settings, "fusion", "RSRS_RECALL_FUSION")?;
    explicit_setting::<f32>(&mut settings, "fusion_alpha", "RSRS_RECALL_FUSION_ALPHA")?;
    explicit_setting::<f32>(&mut settings, "fusion_beta", "RSRS_RECALL_FUSION_BETA")?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_temperature",
        "RSRS_RECALL_FUSION_TEMPERATURE",
    )?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_gate_recency",
        "RSRS_RECALL_FUSION_GATE_RECENCY",
    )?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_gate_recency_temporal",
        "RSRS_RECALL_FUSION_GATE_RECENCY_TEMPORAL",
    )?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_gate_hot",
        "RSRS_RECALL_FUSION_GATE_HOT",
    )?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_gate_tree",
        "RSRS_RECALL_FUSION_GATE_TREE",
    )?;
    explicit_setting::<f32>(
        &mut settings,
        "fusion_temporal_alpha",
        "RSRS_RECALL_FUSION_TEMPORAL_ALPHA",
    )?;
    for (key, name, default) in [
        ("bm25", "RSRS_RECALL_BM25", true),
        ("max_fusion", "RSRS_RECALL_MAX_FUSION", false),
        ("normalize_ablation", "RSRS_RECALL_NORMALIZE_ABLATION", true),
        ("debug_score", "RSRS_DEBUG_SCORE", false),
    ] {
        if crate::env::var_os(name).is_some() {
            settings.insert(key.into(), json!(feedback_switch(name, default)?));
        }
    }
    explicit_setting::<u32>(&mut settings, "heat_cap", "RSRS_RECALL_HEAT_CAP")?;
    explicit_setting::<f32>(&mut settings, "recall_min_score", "RSRS_RECALL_MIN_SCORE")?;
    explicit_setting::<f32>(&mut settings, "mmr_lambda", "RSRS_MMR_LAMBDA")?;
    explicit_setting::<usize>(&mut settings, "ancestor_budget", "RSRS_ANCESTOR_BUDGET")?;
    explicit_setting::<usize>(
        &mut settings,
        "ancestor_root_floor",
        "RSRS_ANCESTOR_ROOT_FLOOR",
    )?;
    settings.insert(
        "now".into(),
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    Ok(settings)
}
