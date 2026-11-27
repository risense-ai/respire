    use super::*;

    fn sample() -> SharePayload {
        SharePayload {
            v: 1,
            kind: "subtree".into(),
            root_title: "\u{7f16}\u{7a0b}\u{5f00}\u{53d1}".into(),
            created_at: "2026-09-21T00:00:00Z".into(),
            device: "test-host/linux".into(),
            source_user: "alice".into(),
            items: vec![
                ShareItem {
                    metadata: Default::default(),
                    unknown_fields: Default::default(),
                    supersedes: String::new(),
                    superseded_by: String::new(),
                    see_also: Vec::new(),
                    id: "aaaa-root".into(),
                    parent_id: "".into(),
                    kind: "context".into(),
                    tags: vec!["rust".into()],
                    title: "\u{6839}\u{6761}".into(),
                    content: "\u{3010}\u{524d}\u{56e0}\u{3011}x\n\u{3010}\u{884c}\u{4e3a}\u{3011}y\n\u{3010}\u{540e}\u{679c}\u{3011}z".into(),
                    created_at: "2026-09-01T00:00:00Z".into(),
                    updated_at: "2026-09-01T00:00:00Z".into(),
                    importance: "important".into(),
                },
                ShareItem {
                    metadata: Default::default(),
                    unknown_fields: Default::default(),
                    supersedes: String::new(),
                    superseded_by: String::new(),
                    see_also: Vec::new(),
                    id: "bbbb-child".into(),
                    parent_id: "aaaa-root".into(),
                    kind: "decision".into(),
                    tags: vec![],
                    title: "\u{5b50}\u{6761}".into(),
                    content: "\u{5b50}\u{6761}\u{6b63}\u{6587}\u{ff0c}\u{542b}\u{4e2d}\u{6587}\u{4e0e}\u{7b26}\u{53f7} \u{2014}\u{2014} \u{6d4b}\u{8bd5}\u{3002}".into(),
                    created_at: "2026-09-02T00:00:00Z".into(),
                    updated_at: "2026-09-02T00:00:00Z".into(),
                    importance: "trivial".into(),
                },
            ],
        }
    }

    #[test]
    fn encode_decode_roundtrip() -> anyhow::Result<()> {
        let p = sample();
        let text = encode(&p)?;
        assert!(text.starts_with(PAYLOAD_PREFIX));
        let back = decode(&text)?;
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.root_title, "\u{7f16}\u{7a0b}\u{5f00}\u{53d1}");
        assert_eq!(back.items[1].parent_id, "aaaa-root");
        assert_eq!(back.items[1].content, "\u{5b50}\u{6761}\u{6b63}\u{6587}\u{ff0c}\u{542b}\u{4e2d}\u{6587}\u{4e0e}\u{7b26}\u{53f7} \u{2014}\u{2014} \u{6d4b}\u{8bd5}\u{3002}");
        assert_eq!(back.total_chars(), p.total_chars());
        Ok(())
    }

    #[test]
    fn decode_accepts_prompt_whole_text() -> anyhow::Result<()> {
        // The AI often saves the whole prompt as a file and feeds it back — must extract the payload from noise
        let p = sample();
        let prompt = build_prompt(&p, &encode(&p)?);
        let back = decode(&prompt)?;
        assert_eq!(back.items.len(), 2);
        Ok(())
    }

    #[test]
    fn decode_rejects_truncated_and_garbage() -> anyhow::Result<()> {
        let p = sample();
        let text = encode(&p)?;
        // Truncate by half → error, not a silent fail
        let cut = &text[..text.len() * 2 / 3];
        assert!(decode(cut).is_err(), "truncated payload must error");
        // no prefix → explicit guidance
        let e = decode("\u{8fd9}\u{662f}\u{4e00}\u{6bb5}\u{65e0}\u{5173}\u{6587}\u{672c}").unwrap_err().to_string();
        assert!(e.contains("payload not found"), "missing prefix must give guidance: {e}");
        // Prefix present but body empty
        assert!(decode(PAYLOAD_PREFIX).is_err());
        Ok(())
    }

    #[test]
    fn decode_rejects_wrong_version() -> anyhow::Result<()> {
        let mut p = sample();
        p.v = 99;
        let text = encode(&p)?;
        let e = decode(&text).unwrap_err().to_string();
        assert!(e.contains("version"), "wrong version must be mentioned: {e}");
        Ok(())
    }

    #[test]
    fn prompt_carries_payload_and_steps() -> anyhow::Result<()> {
        let p = sample();
        let encoded = encode(&p)?;
        let prompt = build_prompt(&p, &encoded);
        assert!(prompt.contains(MARK_BEGIN) && prompt.contains(MARK_END));
        assert!(prompt.contains(&encoded));
        assert!(prompt.contains("2 items"));
        assert!(prompt.contains("share-import"));
        assert!(prompt.contains("--parent"));
        assert!(prompt.contains("--go"));
        assert!(prompt.contains("Conflict gate"), "prompt must mention the conflict gate");
        assert!(prompt.contains("--force"));
        Ok(())
    }

    #[test]
    fn decode_tolerates_line_breaks_in_payload() -> anyhow::Result<()> {
        // Copy-paste often wraps long base64 — must reassemble
        let p = sample();
        let encoded = encode(&p)?;
        let body = encoded.strip_prefix(PAYLOAD_PREFIX).ok_or_else(|| anyhow!("missing prefix"))?;
        let folded: String = body
            .as_bytes()
            .chunks(60)
            .map(|c| format!("{}\n", String::from_utf8_lossy(c)))
            .collect();
        let text = format!("{MARK_BEGIN}\n{PAYLOAD_PREFIX}{folded}{MARK_END}");
        let back = decode(&text)?;
        assert_eq!(back.items.len(), 2);
        Ok(())
    }

    #[test]
    fn decode_survives_prompt_mentioning_prefix_in_prose() -> anyhow::Result<()> {
        // The prompt appendix mentions the prefix as "after `1MEMSHARE1:` is…" — a mention must not mislead
        let p = sample();
        let prompt = build_prompt(&p, &encode(&p)?);
        assert!(prompt.matches(PAYLOAD_PREFIX).count() >= 2, "prompt must mention the prefix more than once (repro)");
        let back = decode(&prompt)?;
        assert_eq!(back.items.len(), 2);
        Ok(())
    }

    #[test]
    fn decode_accepts_bare_folded_payload() -> anyhow::Result<()> {
        // No BEGIN/END markers, bare payload with line wraps — must still work
        let p = sample();
        let encoded = encode(&p)?;
        let body = encoded.strip_prefix(PAYLOAD_PREFIX).ok_or_else(|| anyhow!("missing prefix"))?;
        let folded: String = body
            .as_bytes()
            .chunks(50)
            .map(|c| format!("{}\n", String::from_utf8_lossy(c)))
            .collect();
        let back = decode(&format!("{PAYLOAD_PREFIX}{folded}"))?;
        assert_eq!(back.items.len(), 2);
        Ok(())
    }

    #[test]
    fn decode_stops_at_padding_before_trailing_prose() -> anyhow::Result<()> {
        // A real payload (ending with =) then prose mentioning the prefix — must not swallow the prose into base64
        let p = sample();
        let encoded = encode(&p)?;
        let glued = format!("{encoded}\n\n`{PAYLOAD_PREFIX}` is followed by gzip-compressed JSON.\n");
        let back = decode(&glued)?;
        assert_eq!(back.items.len(), 2);
        Ok(())
    }

    fn item(id: &str, parent: &str) -> ShareItem {
        ShareItem {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id: id.into(),
            parent_id: parent.into(),
            kind: "context".into(),
            tags: vec![],
            title: format!("t-{id}"),
            content: "c".into(),
            created_at: String::new(),
            updated_at: String::new(),
            importance: "trivial".into(),
        }
    }

    #[test]
    fn remap_rebuilds_chain_and_attaches_root() -> anyhow::Result<()> {
        let items = vec![item("r", ""), item("a", "r"), item("b", "a")];
        let new_ids = vec!["N0".to_owned(), "N1".to_owned(), "N2".to_owned()];
        let (parents, re, orph) = remap_parents(&items, &new_ids, "HOST")?;
        assert_eq!(parents, vec!["HOST", "N0", "N1"]);
        assert_eq!(re, 2);
        assert_eq!(orph, 0);
        Ok(())
    }

    #[test]
    fn remap_without_attach_keeps_root_free() -> anyhow::Result<()> {
        let items = vec![item("r", ""), item("a", "r")];
        let new_ids = vec!["N0".to_owned(), "N1".to_owned()];
        let (parents, _, _) = remap_parents(&items, &new_ids, "")?;
        assert_eq!(parents, vec!["", "N0"]);
        Ok(())
    }

    #[test]
    fn remap_rejects_cycle_and_duplicate() {
        let cyc = vec![item("a", "b"), item("b", "a")];
        let ids = vec!["N0".to_owned(), "N1".to_owned()];
        assert!(remap_parents(&cyc, &ids, "").is_err(), "\u{73af}\u{987b}\u{62d2}");
        let dup = vec![item("a", ""), item("a", "")];
        assert!(remap_parents(&dup, &ids, "").is_err(), "\u{91cd}\u{590d} id \u{987b}\u{62d2}");
        assert!(remap_parents(&cyc, &["N0".to_owned()], "").is_err(), "\u{957f}\u{5ea6}\u{4e0d}\u{7b26}\u{987b}\u{62d2}");
    }

    #[test]
    fn remap_treats_missing_parent_as_root() -> anyhow::Result<()> {
        // Parent hand-edited/truncated: treat as a root and count, do not hard-bind into the host library
        let items = vec![item("r", ""), item("a", "gone")];
        let ids = vec!["N0".to_owned(), "N1".to_owned()];
        let (parents, re, orph) = remap_parents(&items, &ids, "HOST")?;
        assert_eq!(parents, vec!["HOST", "HOST"]);
        assert_eq!(re, 0);
        assert_eq!(orph, 1);
        Ok(())
    }

    #[test]
    fn items_from_blanks_parent_outside_subtree() {
        use crate::memory::model::{Kind, MemoryEntry};
        use std::collections::HashSet;
        let mk = |id: &str, parent: &str| MemoryEntry {
            metadata: Default::default(),
            unknown_fields: Default::default(),
            supersedes: String::new(),
            superseded_by: String::new(),
            see_also: Vec::new(),
            id: id.into(),
            kind: Kind::Context,
            tags: vec![],
            title: id.into(),
            content: "c".into(),
            user: "u".into(),
            computer: String::new(),
            project: String::new(),
            created_at: "t".into(),
            updated_at: "t".into(),
            emotion: -1.0,
            parent_id: parent.into(),
            importance: "trivial".into(),
            device: String::new(),
            modified_by: String::new(),
        };
        let entries = [mk("r", "outside"), mk("a", "r")];
        let members: HashSet<String> = ["r", "a"].iter().map(|s| s.to_string()).collect();
        let order: Vec<String> = ["r", "a"].iter().map(|s| s.to_string()).collect();
        let lookup = |id: &str| entries.iter().find(|e| e.id == id).cloned();
        let items = items_from("r", &order, &lookup, &members);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].parent_id, "", "\u{6839}\u{6761}\u{4e4b}\u{7236}\u{5728}\u{5b50}\u{6811}\u{5916}\u{987b}\u{7f6e}\u{7a7a}");
        assert_eq!(items[1].parent_id, "r", "\u{5b50}\u{6811}\u{5185}\u{7236}\u{94fe}\u{987b}\u{4fdd}\u{7559}");
    }

    #[test]
    fn mount_query_includes_root_and_titles() {
        let q = mount_query(&sample());
        assert!(q.contains("\u{7f16}\u{7a0b}\u{5f00}\u{53d1}"));
        assert!(q.contains("\u{6839}\u{6761}"));
        assert!(q.contains("\u{5b50}\u{6761}"));
    }
