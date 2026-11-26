//! The public shape is metadata only. Private custody and dispatch stay native.
use super::*;

const PREFIX_LABEL: &str = "member-";
const MAX_DEVICE_BYTES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InviteRole {
    Editor,
    Viewer,
}

impl InviteRole {
    fn for_readonly(readonly: bool) -> Self {
        if readonly { Self::Viewer } else { Self::Editor }
    }
}

/// Unmounted REST input. The identifier is a device label, not an address,
/// account name, invite code or credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceInviteRequest {
    identifier: String,
    role: InviteRole,
}

impl SpaceInviteRequest {
    pub fn into_host_operation(self) -> Result<HostSpaceOperation> {
        validate_label(&self.identifier)?;
        Ok(HostSpaceOperation::Invite {
            note: Some(self.identifier),
            readonly: self.role == InviteRole::Viewer,
        })
    }
}

fn validate_label(label: &str) -> Result<()> {
    ensure!(!label.is_empty() && label.len() <= MAX_DEVICE_BYTES - PREFIX_LABEL.len()
        && label.trim() == label
        && label.chars().all(|ch| ch.is_alphanumeric() || " _.-".contains(ch)),
        "invalid space member label");
    let folded = label.to_ascii_lowercase();
    let looks_like_factor = folded.strip_prefix("a3-").is_some_and(|suffix| {
        let mut count = 0;
        for byte in suffix.bytes().filter(|byte| *byte != b'-') {
            if !byte.is_ascii_hexdigit() { return false; }
            count += 1;
        }
        count == 36
    });
    ensure!(!looks_like_factor, "invalid space member label");
    Ok(())
}

pub(super) fn member_label(note: Option<&str>) -> String {
    format!("{PREFIX_LABEL}{}", note.unwrap_or("invited").trim())
}

#[derive(Serialize)]
pub struct SpaceMemberView {
    member_id: String,
    display_name: String,
    role: InviteRole,
    status: &'static str,
}

impl SpaceMemberView {
    fn from_row(row: Member) -> Self {
        Self {
            member_id: row.session_id,
            display_name: row.device_name,
            role: InviteRole::for_readonly(row.readonly),
            status: "active",
        }
    }
}

/// Project locally committed session grants; remote revocation is observed by existing control.
/// The caller must hold the selected profile ownership lock.
pub fn list_space_members(profile: &Path) -> Result<Value> {
    let bytes = read_optional(&profile.join(MEMBERS))?;
    let rows = members(bytes.as_deref())?.into_iter().map(SpaceMemberView::from_row).collect::<Vec<_>>();
    Ok(json!({"members": rows}))
}

/// Check the exact terminal native operation and its still-current GET-visible row.
/// The caller must already have consumed the private launch grant and verified
/// the parent and binding, exactly as for control_space. This never takes a
/// private result as input and never returns or queries an invite code.
pub fn committed_space_invite_member(
    ownership: &AuthOwnership,
    operation_id: &str,
    idempotency_key: &str,
    params: &HostSpaceParams,
) -> Result<SpaceMemberView> {
    selection(ownership, operation_id, idempotency_key, params)?;
    let journal = load_journal()?;
    let index = owned_index(&journal, ownership, operation_id, idempotency_key, params)?
        .context("space invite receipt is missing")?;
    let operation = op(&journal, index)?;
    let HostSpaceOperation::Invite { note, readonly } = &operation.intent else {
        anyhow::bail!("space result is not an invite");
    };
    let label = note.as_deref().context("space invite device label is missing")?;
    validate_label(label)?;
    let bytes = read_optional(&ownership.profile.join(MEMBERS))?;
    project_committed(
        journal.operations[index].phase == AuthPhase::Committed,
        operation.session_id.as_deref(),
        &member_label(Some(label)),
        *readonly,
        bytes.as_deref(),
    )
}

fn project_committed(
    committed: bool, session_id: Option<&str>, label: &str, readonly: bool, bytes: Option<&[u8]>,
) -> Result<SpaceMemberView> {
    ensure!(committed, "space invite is not committed");
    let id = session_id.context("space invite member identity missing")?;
    let row = members(bytes)?.into_iter().find(|row| row.session_id == id)
        .context("committed space member is not current")?;
    ensure!(row.device_name == label && row.readonly == readonly, "committed space member changed");
    Ok(SpaceMemberView::from_row(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "12345678-1234-4123-8123-123456789abc";

    fn input(label: &str, role: &str) -> Result<HostSpaceOperation> {
        let request: SpaceInviteRequest = serde_json::from_value(json!({"identifier":label,"role":role}))?;
        request.into_host_operation()
    }

    #[test]
    fn space_invite_projection_accepts_only_bounded_device_labels_and_non_owner_roles() -> Result<()> {
        for (role, readonly) in [("editor", false), ("viewer", true)] {
            let HostSpaceOperation::Invite { note, readonly: actual } = input("Team Tablet-2", role)? else {
                anyhow::bail!("unexpected host operation");
            };
            assert_eq!(member_label(note.as_deref()), "member-Team Tablet-2");
            assert_eq!(actual, readonly);
        }
        for invalid in ["", " ", " leading", "trailing ", "line\nbreak", "1mem-invite:example",
            "A3-000000-000000-000000-000000-000000-000000", "person@example.invalid"] {
            assert!(input(invalid, "viewer").is_err());
        }
        assert!(input("tablet", "owner").is_err());
        assert!(input(&"a".repeat(121), "viewer").is_ok());
        assert!(input(&"a".repeat(122), "viewer").is_err());
        assert!(input(&"\u{732b}".repeat(41), "viewer").is_err());
        assert!(serde_json::from_value::<SpaceInviteRequest>(
            json!({"identifier":"tablet","role":"viewer","invite_code":"private"})).is_err());
        Ok(())
    }

    #[test]
    fn space_invite_projection_matches_get_and_has_no_host_fields() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let fixture = json!([{"session_id":ID,"device_name":"member-tablet","readonly":true,
            "issued_at":"private-timestamp"}]);
        let bytes = serde_json::to_vec(&fixture)?;
        std::fs::write(profile.path().join(MEMBERS), &bytes)?;
        let item = serde_json::to_value(project_committed(true, Some(ID), "member-tablet", true, Some(&bytes))?)?;
        assert_eq!(list_space_members(profile.path())?, json!({"members":[item.clone()]}));
        assert_eq!(item, json!({"member_id":ID,"display_name":"member-tablet","role":"viewer","status":"active"}));
        assert!(project_committed(false, Some(ID), "member-tablet", true, Some(&bytes)).is_err());
        assert!(project_committed(true, None, "member-tablet", true, Some(&bytes)).is_err());
        assert!(project_committed(true, Some(ID), "member-other", true, Some(&bytes)).is_err());
        assert!(project_committed(true, Some(ID), "member-tablet", false, Some(&bytes)).is_err());
        assert!(project_committed(true, Some(ID), "member-tablet", true, None).is_err());
        let mut bad = fixture;
        bad[0]["invite_code"] = json!("synthetic-private-invite");
        assert!(project_committed(true, Some(ID), "member-tablet", true, Some(&serde_json::to_vec(&bad)?)).is_err());
        Ok(())
    }
}
