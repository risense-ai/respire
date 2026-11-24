{
            let session = build_session()?;
            let store = build_local()?;
            let all = scoped_candidates(&store)?;
            let report = respire_app::service::candidate_report(&session, embedder!(), &all, &content)?;
            let mut result = ResultEnvelope::new(
                "candidates",
                OutputStatus::Ok,
                serde_json::json!({"merge_count":report.merge.len(),"parent_count":report.parent.len()}),
                vec![
                    OutputItem::new(
                        "merge candidates",
                        OutputStatus::Ok,
                        report.merge.len().to_string(),
                    ),
                    OutputItem::new(
                        "parent candidates",
                        OutputStatus::Ok,
                        report.parent.len().to_string(),
                    ),
                ],
            );
            result.details = serde_json::json!({"merge":report.merge,"parent":report.parent});
            result.actions.push("candidates <content> --json".into());
            result.actions.extend(respire_app::service::write_decision_actions().iter().map(|action| (*action).to_owned()));
            emit_result(result)?;
}
