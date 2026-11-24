    use super::*;

    fn snapshot(id: &str, project: &str, parent: &str, links: &[&str]) -> Value {
        serde_json::json!({"id":id,"title":id,"deleted":false,"updated_at":"2026-10-24T00:00:00Z",
            "kind":"context","tags":"","project":project,"computer":"test","parent_id":parent,
            "created_at":"2026-10-24T00:00:00Z","content_head":"synthetic","recall_count":0,"importance":"important",
            "features":null,"entry":{"id":id,"kind":"Context","tags":[],"title":id,"content":"synthetic",
                "user":"test","computer":"test","project":project,"created_at":"2026-10-24T00:00:00Z",
                "updated_at":"2026-10-24T00:00:00Z","emotion":0,"parent_id":parent,"importance":"important","see_also":links}})
    }

    fn fixture() -> Value {
        let mut snapshots = Vec::new();
        for index in 0..6 {
            let id = format!("seed-{index}");
            snapshots.push(snapshot(
                &id,
                "allowed",
                if index == 5 { "ancestor" } else { "" },
                if index == 0 {
                    &["seed-5", "ancestor", "outside"]
                } else {
                    &[]
                },
            ));
        }
        snapshots.push(snapshot("ancestor", "allowed", "", &[]));
        snapshots.push(snapshot("outside", "other-project", "", &[]));
        serde_json::json!({"model":"m3","query":{"text":"synthetic","project":"allowed","limit":20},
            "ids":["seed-0","seed-1","seed-2","seed-3","seed-4","seed-5"],"pairs":[],"snapshots":snapshots})
    }

    #[test]
    fn sourcing_floor_and_legacy_mode_preserve_authorized_scope_and_ancestors() -> Result<()> {
        let mut core = Core::new()?;
        let mut payload = fixture();
        let result = core.call("related_business", payload.clone())?;
        assert_eq!(result["related"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["related"][0]["id"], "seed-5");
        payload["related_settings"] = serde_json::json!({"floor":6,"ppr":false});
        assert_eq!(
            core.call("related_business", payload.clone())?["related"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
        payload["related_settings"] = serde_json::json!({"floor":5,"ppr":false});
        assert_eq!(
            core.call("related_business", payload.clone())?["related"][0]["id"],
            "seed-5"
        );
        payload["related_settings"] = serde_json::json!({"max":65});
        assert!(core.call("related_business", payload).is_err());
        Ok(())
    }

    #[test]
    fn graph_bounds_accept_documented_edges_and_reject_excess_before_allocating() -> Result<()> {
        let mut core = Core::new()?;
        for settings in [
            json!({"max":64,"knn_seeds":16,"knn_per":16}),
            json!({"max":0}),
        ] {
            let mut payload = fixture();
            payload["related_settings"] = settings;
            core.call("related_business", payload)?;
        }
        for settings in [
            json!({"max":65}),
            json!({"knn_seeds":17}),
            json!({"knn_per":17}),
        ] {
            let mut payload = fixture();
            payload["related_settings"] = settings;
            let error = core
                .call("related_business", payload)
                .err()
                .ok_or_else(|| anyhow::anyhow!("excess graph setting accepted"))?;
            assert!(error.to_string().contains("bounded graph limits"));
        }
        let mut payload = fixture();
        payload["ids"] = json!((0..256)
            .map(|index| format!("node-{index}"))
            .collect::<Vec<_>>());
        payload["snapshots"] = Value::Array(
            (0..256)
                .map(|index| snapshot(&format!("node-{index}"), "allowed", "", &[]))
                .collect(),
        );
        core.call("related_business", payload.clone())?;
        payload["ids"]
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("ids are not an array"))?
            .push(json!("excess"));
        let error = core
            .call("related_business", payload)
            .err()
            .ok_or_else(|| anyhow::anyhow!("excess association seeds accepted"))?;
        assert!(error.to_string().contains("seed limit is 256"));
        Ok(())
    }
