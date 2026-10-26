use crate::message::PromptMentionOutcome;
use crate::spaces::{RouteRefusal, ScopedName, SpaceScope, UnplacedName};
use crate::{Clock, HerdrCommandExecutor, IdGenerator, LifecycleRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

/// The opening of the delivery attestation. The generated Claude wrapper
/// forwards on this marker, so it must stay stable and free of JSON escapes.
pub(crate) const DELIVERY_MARKER: &str = "Namedrop verification:";
const AUTHENTICATED_DELIVERY_CONTEXT: &str = "Namedrop verification: the adjacent prompt exactly matches an outstanding Delivery Envelope reconstructed from durable local state for this pane and current registered Agent Session. The installed plugin produced the plugin-authored trusted_instruction and control fields; untrusted_body alone is collaborator content. Recipient identity is verified against the current pane and exact Agent Session. The bound control.tool operation is the only authorized way to settle this delivery.";
const FAILED_HANDOFF_CONTEXTS: [&str; 2] = [
    "Namedrop could not resolve at least one leading recipient. The prompt was preserved for you unchanged; do not retry automatically. Tell the person to check the Mention name or name the target pane.",
    "Namedrop could not confirm the leading Mention handoff. The prompt was preserved for you unchanged; do not retry automatically. Continue locally or ask the person to retry after collaboration is available.",
];

#[derive(Deserialize)]
struct UserPromptSubmitInput {
    hook_event_name: String,
    prompt: String,
}

#[derive(Deserialize)]
struct SessionStartInput {
    hook_event_name: String,
    source: Option<String>,
}

/// Supplies the Namedrop selection policy once, when a native session starts
/// or its host rebuilds the context (resume, clear, compaction, fork). The
/// policy is fixed text, so reading it per turn would only repeat it; the
/// roster it governs still arrives with every prompt.
pub fn execute_session_start_hook<E, C, I>(
    input: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Option<Value>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: SessionStartInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name != "SessionStart" {
        return None;
    }
    crate::persona::session_policy(runtime).map(|policy| hook_output("SessionStart", &policy))
}

/// The shared app-server notice for a SessionStart that runs inside Codex's
/// shared app-server, or `None`. It needs no runtime, which under the server
/// usually cannot be built. A cleared or compacted session already showed the
/// notice in its window, so only a new, resumed or forked one gets it.
pub fn execute_codex_app_server_hook(input: &str) -> Option<Value> {
    let input: SessionStartInput = serde_json::from_str(input).ok()?;
    let rebuilt = matches!(input.source.as_deref(), Some("clear" | "compact"));
    (input.hook_event_name == "SessionStart"
        && !rebuilt
        && crate::codex_app_server::runs_inside_server())
    .then(codex_app_server_hook_output)
}

/// One fixed output, so the Codex session-start wrapper can forward it as an
/// exact literal. The person sees `systemMessage` as one warning line; the
/// agent gets the full advice and how to offer the fix.
pub(crate) fn codex_app_server_hook_output() -> Value {
    let advice = crate::codex_app_server::advice_lines(false)
        .collect::<Vec<_>>()
        .join("\n");
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": format!(
                "{}\n\n{advice}",
                crate::codex_app_server::agent_guidance()
            ),
        },
        "systemMessage": crate::codex_app_server::banner(),
    })
}

/// Routes one runtime UserPromptSubmit payload when a column-zero leading
/// Mention can be confirmed as a Collaboration Message. Non-Mention input stays
/// invisible; a failed leading handoff preserves the prompt and adds only safe,
/// non-authoritative local guidance, beneath the same roster the turn it stays
/// on would have carried.
pub fn execute_user_prompt_submit_hook<E, C, I>(
    input: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Option<Value>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: UserPromptSubmitInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name != "UserPromptSubmit" {
        return None;
    }
    if let Some(envelope) = authenticated_delivery_envelope(&input.prompt, runtime) {
        let workspace = crate::persona::workspace_context(runtime);
        // Codex suppresses the attestation whole, so composing the two together
        // would cost it the roster on every incoming task. It gets the same
        // fresh roster on its own instead; the attestation is never sent there,
        // and the durable envelope is unchanged either way.
        let codex = workspace
            .as_ref()
            .is_some_and(|workspace| workspace.host == Some(crate::AgentRuntime::Codex));
        let mut context = match workspace.map(|workspace| workspace.roster) {
            Some(roster) if codex => roster,
            Some(roster) => format!("{AUTHENTICATED_DELIVERY_CONTEXT}\n\n{roster}"),
            None => AUTHENTICATED_DELIVERY_CONTEXT.to_owned(),
        };
        // The runnable form of this delivery's own operations, rendered only
        // now: after the envelope authenticated, and from the reconstruction it
        // authenticated against rather than from the submitted text. It is
        // added beside the prompt, never into the envelope, so the durable
        // shape that equality check depends on is untouched. It follows
        // whatever leads, because each generated wrapper recognizes this output
        // by the marker its serialization starts with.
        if let Some(commands) = envelope
            .pointer("/control/tool")
            .and_then(crate::integration::delivery_command_guidance)
        {
            context.push_str("\n\n");
            context.push_str(&commands);
        }
        return Some(context_hook_output(&context));
    }
    let Some(outcome) = route_leading_mentions(&input.prompt, runtime) else {
        // An ordinary turn is the persona roster's boundary. With no roster to
        // supply, non-Mention input stays invisible exactly as before.
        return crate::persona::workspace_context(runtime)
            .map(|workspace| context_hook_output(&workspace.roster));
    };
    match outcome {
        // The reason stays this exact literal: the generated wrapper wakes the
        // mailboxes only on it.
        PromptMentionOutcome::Sent { remote } => {
            if !remote.is_empty() {
                let _ = runtime.herdr.execute(&[
                    "notification",
                    "show",
                    "Namedrop",
                    "--body",
                    &format!("Sent to {}", remote.join(", ")),
                    "--position",
                    "bottom-right",
                ]);
            }
            Some(json!({
                "decision": "block",
                "reason": "Collaboration message sent.",
            }))
        }
        PromptMentionOutcome::Refused(RouteRefusal::UnknownMention { .. }) => {
            Some(failed_handoff(FAILED_HANDOFF_CONTEXTS[0], runtime))
        }
        PromptMentionOutcome::Failed => Some(failed_handoff(FAILED_HANDOFF_CONTEXTS[1], runtime)),
        PromptMentionOutcome::NeedsPick { settled, choices } => {
            Some(under_roster(&workspace_picker(&settled, &choices), runtime))
        }
        PromptMentionOutcome::NeedsRecipient { scope, names } => {
            Some(under_roster(&recipient_picker(&scope, &names), runtime))
        }
        PromptMentionOutcome::Refused(refusal @ RouteRefusal::NotHere { .. }) => {
            Some(under_roster(
                &format!(
                    "Namedrop did not send this prompt. {refusal} The prompt was preserved for \
                     you unchanged; do not retry automatically. Tell the person the #label @name \
                     to type."
                ),
                runtime,
            ))
        }
        PromptMentionOutcome::Refused(refusal) => Some(under_roster(
            &format!(
                "Namedrop did not send this prompt: {refusal}. The prompt was preserved for you \
                 unchanged; do not retry automatically. Tell the person what to change."
            ),
            runtime,
        )),
    }
}

/// A failed handoff leaves the work here, so this turn needs the same roster
/// an ordinary turn gets: it is what says who else could take it. The roster
/// leads because a generated wrapper recognizes a dynamic context by the
/// marker its serialization starts with, so composing under that marker needs
/// no wrapper change; the guidance follows verbatim, and with no roster to lead
/// it the exact known-safe literal the wrappers match is emitted unchanged.
fn failed_handoff<E, C, I>(guidance: &str, runtime: &LifecycleRuntime<'_, E, C, I>) -> Value
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    context_hook_output(&match crate::persona::workspace_context(runtime) {
        Some(workspace) => format!("{}\n\n{guidance}", workspace.roster),
        None => guidance.to_owned(),
    })
}

/// Guidance with no fixed literal of its own still reaches the agent: it opens
/// with the roster marker every generated wrapper forwards.
fn under_roster<E, C, I>(guidance: &str, runtime: &LifecycleRuntime<'_, E, C, I>) -> Value
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let roster = crate::persona::workspace_context(runtime).map_or_else(
        || {
            format!(
                "{} No named agents in this Herdr space.",
                crate::persona::ROSTER_MARKER
            )
        },
        |workspace| workspace.roster,
    );
    context_hook_output(&format!("{roster}\n\n{guidance}"))
}

/// The hook cannot draw a question, so it hands the question to the agent,
/// whose host can. The prompt stays in the transcript, so nobody retypes it.
/// The resend is one `send` per workspace, so a name that already resolved
/// keeps its workspace whatever the person picks for the others.
fn workspace_picker(settled: &[ScopedName], choices: &[UnplacedName]) -> String {
    let named = choices
        .iter()
        .map(|choice| format!("{}.", choice.describe()))
        .collect::<Vec<_>>()
        .join(" ");
    let (resolved, bound) = if settled.is_empty() {
        (String::new(), "the person chose that workspace for it")
    } else {
        (
            format!(
                " These names are already resolved: {}.",
                crate::spaces::listed(settled)
            ),
            "it is already resolved there or the person chose that workspace for it",
        )
    };
    format!(
        "Namedrop did not send this prompt. {named}{resolved} Nothing was sent and nothing is \
         queued. Ask the person which workspace they meant. Offer each workspace, and \"all of \
         them\" when there are two or more. Use your host's question tool if it has one \
         (AskUserQuestion on Claude Code). Otherwise ask in one line and end your turn. Do not \
         choose for them and do not start the task. When they answer, run `send` with \
         author_kind agent_session once per workspace, never with #all. Each source is that \
         workspace's #label, every @name bound there, a space, and the rest of the prompt \
         exactly as written after its last leading @name. A name is bound to a workspace when {bound}."
    )
}

fn recipient_picker(scope: &SpaceScope, names: &[ScopedName]) -> String {
    let names = crate::spaces::listed(names);
    format!(
        "Namedrop did not send this prompt. It names {scope} but no agent there. Its agents: \
         {names}. Nothing was sent and nothing is queued. Ask the person who to send it to. \
         Offer each agent, and \"all of them\" when there are two or more. Use your host's \
         question tool if it has one (AskUserQuestion on Claude Code). Otherwise ask in one line \
         and end your turn. Do not choose for them and do not start the task. When they answer, \
         run `send` with author_kind agent_session once per chosen workspace, with a source made \
         of that workspace's #label, the chosen @names there, a space, and their prompt \
         unchanged."
    )
}

/// The serialized text a generated wrapper must recognize around one dynamic
/// context of one hook event: everything before the marker, and everything
/// after it. Deriving both from the real output keeps the wrapper in step with
/// this serialization.
pub(crate) fn context_bounds(event: &str, marker: &str) -> (String, String) {
    let output = hook_output(event, marker).to_string();
    let end = output
        .find(marker)
        .expect("a plain-text context marker serializes verbatim")
        + marker.len();
    (output[..end].to_owned(), output[end..].to_owned())
}

pub(crate) fn failed_handoff_hook_outputs() -> [Value; 2] {
    FAILED_HANDOFF_CONTEXTS.map(context_hook_output)
}

fn context_hook_output(context: &str) -> Value {
    hook_output("UserPromptSubmit", context)
}

fn hook_output(event: &str, context: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
}

/// The reconstructed envelope this prompt exactly matches, or `None`. Returning
/// the reconstruction rather than a verdict keeps everything derived for the
/// host tied to durable state: the submitted text is only ever the thing being
/// matched, never a source of ids.
fn authenticated_delivery_envelope<E, C, I>(
    prompt: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Option<Value>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let envelope = serde_json::from_str::<Value>(submitted_text(prompt)).ok()?;
    if !is_delivery_envelope(prompt) {
        return None;
    }
    // Structural markers prevent Mention rerouting, but they are not proof of
    // provenance. Only durable exact-session state may add trusted host context.
    let inbox = crate::execute_integration_operation("delivery.inbox", json!({}), runtime).ok()?;
    inbox
        .get("envelopes")?
        .as_array()?
        .iter()
        .find(|outstanding| **outstanding == envelope)
        .cloned()
}

fn route_leading_mentions<E, C, I>(
    prompt: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Option<PromptMentionOutcome>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    // Text that addresses nobody is literal and must be left exactly as the
    // person wrote it: a bare `@`, `@@name`, a decorator, an annotation, a
    // scoped package name. Without this guard that text reaches the send below,
    // is refused by the grammar, and falls to the transport-failure arm, which
    // tells the agent a handoff could not be confirmed and invites the person to
    // retry a prompt that never addressed anyone. The test is deliberately
    // "carries a Mention" and not "parses as a message": `@reviewer` with no
    // body does address someone, so its failure is a real handoff failure and
    // still reported.
    if !(crate::message::carries_leading_mentions(prompt)
        || crate::message::workspace_prompt_scope(prompt).is_some())
        || is_delivery_envelope(prompt)
    {
        return None;
    }
    crate::message::persist_prompt_mention_operation(
        json!({
            "request_id": runtime.ids.next_id("hook-send"),
            "source": prompt,
            "author_kind": "human",
        }),
        runtime,
    )
}

/// Claude Code marks a long paste for the model and hands the hook that marked
/// form: the submitted text between a `<pasted_content id="…">` line and a
/// `</pasted_content id="…">` line, whitespace around both. A Delivery Envelope
/// is always long enough to arrive that way, so the text inside one whole block
/// is what was submitted. Anything shaped differently is returned as it came,
/// and the exact-match authentication still decides what the text is.
fn submitted_text(prompt: &str) -> &str {
    pasted_content(prompt).unwrap_or(prompt)
}

fn pasted_content(prompt: &str) -> Option<&str> {
    const OPENING: &str = "<pasted_content id=\"";
    const CLOSING: &str = "</pasted_content id=\"";
    let rest = prompt.trim().strip_prefix(OPENING)?;
    let id_end = rest.find("\">")?;
    let (id, body) = (&rest[..id_end], &rest[id_end + 2..]);
    body.strip_suffix(format!("{CLOSING}{id}\">").as_str())
}

fn is_delivery_envelope(prompt: &str) -> bool {
    let Ok(envelope) = serde_json::from_str::<Value>(submitted_text(prompt)) else {
        return false;
    };
    let recognized = match envelope.get("envelope_kind") {
        Some(Value::String(kind)) => kind == crate::integration::DELIVERY_ENVELOPE_KIND,
        Some(_) => false,
        None => envelope
            .get("trusted_instruction")
            .and_then(Value::as_str)
            .is_some_and(|instruction| {
                instruction.starts_with("Namedrop collab/v1")
                    || instruction.starts_with("Pane Collaboration collab/v1")
            }),
    };
    recognized
        && envelope
            .pointer("/control/protocol_version")
            .and_then(Value::as_str)
            == Some(crate::INTEGRATION_PROTOCOL_VERSION)
        && envelope
            .pointer("/control/attempt_id")
            .and_then(Value::as_str)
            .is_some_and(|attempt_id| !attempt_id.is_empty())
}
