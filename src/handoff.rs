use crate::{LifecycleRuntime, StateStore};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusInput {
    #[serde(default)]
    include_completed: bool,
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

fn default_limit() -> u32 {
    50
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultsInput {
    message_id: Option<String>,
    coordination_id: Option<crate::CoordinationId>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreviewMode {
    #[default]
    Send,
    DirectPrompt,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewInput {
    source: String,
    #[serde(default)]
    mode: PreviewMode,
    context: Option<crate::context::MessageContextInput>,
}

/// Read request progress and replies without running recovery or dispatch.
pub fn execute_handoff_operation<H, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, H, C, I>,
) -> Result<Value, String>
where
    H: crate::Host,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    if matches!(
        operation,
        "handoff.begin" | "handoff.resolve" | "handoff.adopt"
    ) {
        return crate::coordination::execute(operation, input, runtime);
    }
    if operation == "handoff.preview" {
        return preview(input, runtime);
    }
    let mut coordination_id = None;
    let (status, message_id) = match operation {
        "handoff.status" => {
            let status: StatusInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            if !(1..=100).contains(&status.limit) {
                return Err("limit must be between 1 and 100".to_owned());
            }
            (Some(status), None)
        }
        "handoff.results" => {
            let input: ResultsInput =
                serde_json::from_value(input).map_err(crate::input_schema_error)?;
            if input.message_id.is_some() == input.coordination_id.is_some()
                || input
                    .message_id
                    .as_ref()
                    .is_some_and(|id| id.trim().is_empty())
            {
                return Err(
                    "exactly one nonempty message_id or coordination_id is required".to_owned(),
                );
            }
            coordination_id = input.coordination_id;
            (None, input.message_id.map(crate::MessageId::new))
        }
        _ => return Err(format!("unsupported handoff operation: {operation}")),
    };
    let discovered = runtime.host.space(runtime.space_locator(), None)?;
    let namespace = discovered.namespace(runtime.session_socket_id);
    let pane = runtime
        .current_pane_id
        .ok_or("the current Herdr pane is required")?;
    let mut store = StateStore::open_read_only(runtime.state_directory)
        .map_err(|e| e.to_string())?
        .ok_or("Namedrop has no saved state")?;
    let remote = store
        .transaction(|tx| tx.queued_remote_seat_spaces(&namespace))
        .map_err(|e| e.to_string())?;
    let live = crate::spaces::LiveSpaces::load(&runtime.host, discovered.clone(), &remote);
    store.transaction(|tx| {
        let author = crate::context::registered_current_address(tx, &namespace, pane, &discovered)?;
        if let Some(id) = coordination_id { return tx.coordination_report(&namespace,&id,&author); }
        if let Some(message_id) = message_id {
            let mut report = tx.handoff_report(&namespace, &author, &message_id, runtime.clock.now_unix_ms(), true)?;
            tx.add_handoff_outlooks(&namespace, &live, &mut report)?;
            for recipient in report["recipients"].as_array_mut().unwrap() {
                if recipient["result_status"]=="replied" {
                    let address = crate::PaneAddressId::new(recipient["target"]["address_id"].as_str().unwrap());
                    match tx.handoff_review_snapshot(&namespace,&message_id,&address)? {
                        None => {
                            recipient["reviewed_git_state"] = Value::Null;
                            recipient["freshness"] = json!({"status":"unknown","reason":"no_attached_review_context"});
                        }
                        Some(snapshot) => {
                            let checkout = crate::context::resolve_saved_checkout(&snapshot, &discovered);
                            recipient["reviewed_checkout"] = checkout.public_summary();
                            recipient["reviewed_git_state"] = snapshot["git_state"].clone();
                            recipient["freshness"] = crate::context::review_freshness(&checkout, &snapshot);
                        }
                    }
                }
            }
            let links = tx.coordination_links(&namespace, &message_id)?;
            if let Some(link)=links.first() { report["coordination"] = tx.coordination_report(&namespace,&link.coordination_id,&author)?; }
            Ok(report)
        } else {
            let status = status.expect("status input was parsed");
            let mut ids = tx.handoff_message_ids(&namespace, &author, status.include_completed, status.limit + 1, status.offset)?;
            let mut coordinations = tx.coordination_reports(&namespace,&author,status.include_completed,status.limit + 1,status.offset)?;
            let has_more = ids.len() > status.limit as usize || coordinations.len() > status.limit as usize;
            coordinations.truncate(status.limit as usize);
            ids.truncate(status.limit as usize);
            let reports = ids.iter().map(|id| {
                let mut report = tx.handoff_report(&namespace, &author, id, runtime.clock.now_unix_ms(), false)?;
                tx.add_handoff_outlooks(&namespace, &live, &mut report)?;
                Ok(report)
            })
                .collect::<Result<Vec<_>, crate::StoreError>>()?;
            let adoptable = tx.adoptable_coordinations(&namespace, &author)?;
            Ok(json!({"handoffs":reports, "coordinations":coordinations, "adoptable":adoptable, "has_more":has_more, "next_offset":has_more.then_some(status.offset as u64 + status.limit as u64)}))
        }
    }).map_err(|e| e.to_string())
}

fn preview<H, C, I>(input: Value, runtime: &LifecycleRuntime<'_, H, C, I>) -> Result<Value, String>
where
    H: crate::Host,
    C: crate::Clock,
    I: crate::IdGenerator,
{
    let input: PreviewInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    let parsed = match input.mode {
        PreviewMode::Send => crate::parse_mentions(&input.source)?,
        PreviewMode::DirectPrompt => {
            if input.context.is_some() {
                return Err("direct prompts do not attach a context block".to_owned());
            }
            crate::parse_prompt_mentions(&input.source)?
        }
    };
    if parsed.recipients.scope != crate::SpaceScope::Unqualified {
        return Err(
            "preview covers this workspace only; send a #workspace message directly".to_owned(),
        );
    }
    // The Space is resolved before the context block is previewed, so the
    // preview answers for the same context root the send would capture against.
    let discovered = runtime.host.space(runtime.space_locator(), None)?;
    let context = input
        .context
        .map(|c| crate::context::preview_message_context(c, &discovered))
        .transpose()?;
    let namespace = discovered.namespace(runtime.session_socket_id);
    let pane = runtime
        .current_pane_id
        .ok_or("the current Herdr pane is required")?;
    let mut store = StateStore::open_read_only(runtime.state_directory)
        .map_err(|e| e.to_string())?
        .ok_or("Namedrop has no saved state")?;
    let reference_aliases = crate::message::body_reference_aliases(
        &parsed,
        &crate::spaces::HerdrWorkspaces::new(&runtime.host),
    );
    store.transaction(|tx| {
        let author = crate::context::registered_current_address(tx,&namespace,pane,&discovered)?;
        let recipients = tx.resolve_message_recipients(&namespace,&parsed.recipients.aliases,&author.address_id)?;
        let recipients = recipients.iter().map(|r|tx.recipient_preview(&namespace,&discovered,&r.address_id,None)).collect::<Result<Vec<_>,_>>()?;
        let references = reference_aliases.iter().map(|alias| {
            let address = tx.resolve_routing_alias(&namespace,alias)?;
            Ok(json!({"address_id":address.address_id,"alias":address.alias}))
        }).collect::<Result<Vec<_>,crate::StoreError>>()?;
        Ok(json!({"recipients":recipients,"context_references":references,"authored_source":input.source,"message":parsed.body,"context":context,
            "notice":"Preview only. Nothing was sent or shared; sending resolves recipients and delivery conditions again."}))
    }).map_err(|e|e.to_string())
}
