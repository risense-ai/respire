use super::*;

type ProofKey<'a> = (&'a str, &'a str, &'a str, &'a str);
type RootKey<'a> = (&'a str, &'a str, &'a str);

struct AuthorityView<'a> {
    check: &'a AuthorityCheck,
    roots: HashMap<RootKey<'a>, &'a RootOutcome>,
}

fn authority_index(
    checks: &[AuthorityCheck],
) -> Result<HashMap<ProofKey<'_>, AuthorityView<'_>>, MetadataError> {
    let mut result = HashMap::with_capacity(checks.len());
    for check in checks {
        validate_full_id(&check.id)?;
        validate_revision(&check.record_revision)?;
        validate_revision(&check.claim_digest)?;
        validate_extra_fields(&check.confirmed_by.unknown_fields)?;
        validate_timestamp(&check.confirmed_at)?;
        if check.confirmed_by.id.trim().is_empty() || check.confirmed_by.event_id != check.event_id
        {
            return Err(invalid("authority subject or event binding is invalid"));
        }
        if check.event_id.trim().is_empty() {
            return Err(invalid("authority event ID cannot be empty"));
        }
        let mut roots = HashMap::with_capacity(check.roots.len());
        for root in &check.roots {
            if !matches!(root.kind.as_str(), "user" | "primary" | "run")
                || root.locator.trim().is_empty()
            {
                return Err(invalid("invalid root evidence identity"));
            }
            validate_revision(&root.revision)?;
            if roots
                .insert(
                    (
                        root.kind.as_str(),
                        root.locator.as_str(),
                        root.revision.as_str(),
                    ),
                    &root.outcome,
                )
                .is_some()
            {
                return Err(invalid("duplicate root evidence check"));
            }
        }
        let key = (
            check.id.as_str(),
            check.record_revision.as_str(),
            check.event_id.as_str(),
            check.claim_digest.as_str(),
        );
        if result.insert(key, AuthorityView { check, roots }).is_some() {
            return Err(invalid("duplicate authority check"));
        }
    }
    Ok(result)
}

fn scope_reasons(scope: &Scope, request: &RequestScope) -> BTreeSet<&'static str> {
    let mut reasons = BTreeSet::new();
    for (allowed, actual) in [
        (&scope.project, &request.project),
        (&scope.device, &request.device),
        (&scope.version, &request.version),
    ] {
        match allowed.as_deref() {
            None => {
                reasons.insert("scope_unknown");
            }
            Some("*") => {}
            Some(value) if actual.as_deref() == Some(value) => {}
            Some(_) => {
                reasons.insert("scope_mismatch");
            }
        }
    }
    for (boundary, lower) in [(&scope.from, true), (&scope.until, false)] {
        match boundary.as_deref() {
            None => {
                reasons.insert("scope_unknown");
            }
            Some("*") => {}
            Some(value)
                if (lower && request.at.as_str() < value)
                    || (!lower && request.at.as_str() >= value) =>
            {
                reasons.insert("scope_mismatch");
            }
            Some(_) => {}
        }
    }
    reasons
}

struct NodeState {
    revision: Option<String>,
    evidence_state: EvidenceState,
    reasons: BTreeSet<&'static str>,
    local_eligible: bool,
    root_available: bool,
    fact_eligible: bool,
}

/// Recomputes effective evidence for one authorized, immutable request snapshot.
pub fn evaluate(
    records: &[MetadataRecord<'_>],
    request: &RequestScope,
    now: &str,
    authority_checks: &[AuthorityCheck],
    unavailable_ids: &[String],
) -> Result<Vec<EvidenceEvaluation>, MetadataError> {
    validate_request_scope(request)?;
    validate_timestamp(now)?;
    if request.at != now {
        return Err(invalid("request scope time must equal now"));
    }
    let indices = record_index(records)?;
    let authority = authority_index(authority_checks)?;
    let unavailable: HashSet<_> = unavailable_ids.iter().map(String::as_str).collect();
    let edges = dependency_graph(records, &indices);
    let (order, cycles) = graph_order(&edges);
    let mut states = Vec::with_capacity(records.len());

    for (index, record) in records.iter().enumerate() {
        let mut state = NodeState {
            revision: None,
            evidence_state: EvidenceState::Unverified,
            reasons: BTreeSet::new(),
            local_eligible: true,
            root_available: false,
            fact_eligible: false,
        };
        if record.deleted {
            state.reasons.insert("deleted");
            state.local_eligible = false;
        }
        if record.superseded {
            state.reasons.insert("superseded");
            state.local_eligible = false;
        }
        if unavailable.contains(record.id) || record.content.is_none() {
            state.reasons.insert("source_unavailable");
            state.local_eligible = false;
        }
        if cycles[index] {
            state.reasons.insert("cycle");
            state.local_eligible = false;
        }
        match validate_body(record) {
            Err(error) if error.code == "unsupported_metadata" => {
                state.reasons.insert("unsupported_metadata");
                state.reasons.insert("unverified");
                state.local_eligible = false;
                states.push(state);
                continue;
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        if record.content.is_some() {
            state.revision = Some(record_revision(record)?);
        }
        if record
            .metadata
            .valid_until
            .as_deref()
            .is_some_and(|until| now >= until)
        {
            state.reasons.insert("expired");
            state.local_eligible = false;
        }
        if record
            .metadata
            .review_at
            .as_deref()
            .is_some_and(|review_at| now >= review_at)
        {
            state.reasons.insert("review_due");
        }
        if let Some(verification) = &record.metadata.verification {
            let current_revision = state.revision.as_deref();
            let same_revision = current_revision == Some(verification.record_revision.as_str());
            if !same_revision {
                state.reasons.insert("verification_revision_mismatch");
            }
            let same_sources = evidence_set_matches(record.metadata, &verification.evidence);
            let claim = claim_digest(
                record.id,
                &verification.record_revision,
                &verification.claim,
                &verification.scope,
                &verification.evidence,
                &verification.method,
            )?;
            let key = (
                record.id,
                verification.record_revision.as_str(),
                verification.confirmed_by.event_id.as_str(),
                claim.as_str(),
            );
            let proof = authority.get(&key);
            let same_subject = proof.is_some_and(|value| {
                value.check.confirmed_by == verification.confirmed_by
                    && value.check.confirmed_at == verification.confirmed_at
            });
            match proof.map(|value| &value.check.outcome) {
                Some(AuthorityOutcome::Trusted)
                    if same_revision && same_sources && same_subject =>
                {
                    state.evidence_state = EvidenceState::Verified;
                }
                Some(AuthorityOutcome::Rejected) => {
                    state.reasons.insert("authority_rejected");
                }
                Some(AuthorityOutcome::Trusted) if !same_subject => {
                    state.reasons.insert("authority_rejected");
                }
                Some(AuthorityOutcome::Trusted) => {}
                _ => {
                    state.reasons.insert("authority_unavailable");
                }
            }
            let scope = scope_reasons(&verification.scope, request);
            if !scope.is_empty() {
                state.local_eligible = false;
                state.reasons.extend(scope);
            }
            for evidence in &verification.evidence {
                let (kind, locator, revision) = evidence_parts(evidence);
                if kind == "memory" {
                    continue;
                }
                let root = proof
                    .and_then(|value| value.roots.get(&(kind, locator, revision)))
                    .copied();
                match root {
                    Some(RootOutcome::Available) => state.root_available = true,
                    Some(RootOutcome::Changed) => {
                        state.reasons.insert("root_changed");
                        state.local_eligible = false;
                    }
                    _ => {
                        state.reasons.insert("root_unavailable");
                        state.local_eligible = false;
                    }
                }
            }
        }
        if state.evidence_state == EvidenceState::Unverified {
            state.reasons.insert("unverified");
            state.local_eligible = false;
        }
        states.push(state);
    }

    for (index, record) in records.iter().enumerate() {
        for source in record
            .metadata
            .source_refs
            .iter()
            .filter(|source| source.required)
        {
            let reason = match indices.get(source.id.as_str()).copied() {
                None => Some("source_unavailable"),
                Some(target)
                    if unavailable.contains(source.id.as_str())
                        || records[target].content.is_none() =>
                {
                    Some("source_unavailable")
                }
                Some(target)
                    if states[target].revision.as_deref() != Some(source.revision.as_str()) =>
                {
                    Some("source_revision_mismatch")
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                states[index].reasons.insert(reason);
                states[index].local_eligible = false;
            }
        }
    }

    for index in order {
        let sources_valid = edges[index]
            .iter()
            .all(|&source| states[source].fact_eligible);
        let root_available = states[index].root_available
            || edges[index]
                .iter()
                .any(|&source| states[source].root_available);
        let state = &mut states[index];
        if !sources_valid && !cycles[index] {
            state.reasons.insert("source_invalid");
        }
        if !root_available && state.evidence_state == EvidenceState::Verified {
            state.reasons.insert("root_unavailable");
        }
        state.root_available = root_available;
        state.fact_eligible = state.local_eligible && sources_valid && root_available;
    }

    Ok(records
        .iter()
        .zip(states)
        .map(|(record, state)| {
            let due_reasons = if record.deleted || record.superseded {
                Vec::new()
            } else {
                state
                    .reasons
                    .iter()
                    .filter_map(|reason| match *reason {
                        "review_due" | "unverified" | "expired" | "source_unavailable"
                        | "source_invalid" | "cycle" => Some((*reason).to_owned()),
                        "source_revision_mismatch" | "root_changed" | "root_unavailable" => {
                            Some("source_invalid".to_owned())
                        }
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            };
            EvidenceEvaluation {
                id: record.id.to_owned(),
                record_revision: state.revision,
                evidence_state: state.evidence_state,
                fact_eligible: state.fact_eligible,
                due_reasons,
                reason_codes: state.reasons.into_iter().map(str::to_owned).collect(),
            }
        })
        .collect())
}
