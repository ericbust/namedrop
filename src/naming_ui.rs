//! Terminal popups; operations own naming rules and health diagnostics.
use super::popup_input::InputMode;
use super::*;
use crate::agent_status_ui::{Tone, sanitize, tint, wrap};
use namedrop::{
    DiscoveredPane, HealthStatus, MAX_PERSONA_CHARS, PaneAddressRecord, PaneType,
    cli::UiMode,
    tour::{self, PageAction, Presentation},
};
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

/// Columns the popups wrap their prose to. Herdr is asked for 72 and a popup
/// can be clamped narrower, so this leaves the same margin the doctor page
/// already keeps.
const POPUP_COLUMNS: usize = 68;

/// Wrapped lines one block of persona text may occupy. A migrated description
/// can run to many lines, and neither the draft nor a suggestion may push the
/// editor's own controls out of the popup.
const MAX_PERSONA_LINES: usize = 4;

/// Doctor report lines per page. The page, its controls and the popup's
/// border fit a 24-row popup with room to spare.
const DOCTOR_PAGE_LINES: usize = 16;

/// Longest a non-interactive run defers a scripted key while a suggestion is
/// still in flight. The interactive editor never waits: it polls.
const SUGGESTION_WAIT: Duration = Duration::from_secs(20);

pub(super) fn install_shortcuts(
    config: Option<&Path>,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<serde_json::Value, String> {
    let captured = environment
        .agent_config()
        .shortcut_config(environment.home());
    namedrop::install_shortcuts_at(config.or(captured.as_deref()))
}

/// Clears the once-only marker so the current tour opens again the next time
/// Namedrop sees an agent pane.
pub(super) fn reset_tour(options: &RuntimeOptions) -> Result<serde_json::Value, String> {
    with_runtime_options(options, |runtime| {
        let cleared = tour::reset(runtime.state_directory, tour::CURRENT_ID)?;
        Ok(serde_json::json!({
            "presentation_id": tour::CURRENT_ID,
            "cleared": cleared,
        }))
    })
}

pub(super) fn launch(options: &RuntimeOptions, mode: UiMode) -> Result<serde_json::Value, String> {
    with_runtime_options(options, |runtime| {
        let workspace = runtime
            .workspace_id
            .ok_or("This popup requires an active Herdr space")?;
        let pane = runtime
            .current_pane_id
            .ok_or("This popup requires an active Herdr pane")?;
        let origin = target_pane(runtime)?;
        if mode == UiMode::Tour {
            return tour::open_popup(runtime, &origin);
        }
        let terminal = origin.terminal_id;
        // The popup carries the Agent Session it was opened for, so asking for
        // help later cannot reach a replacement session in the same pane.
        let session = origin
            .agent_session_id
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default();
        runtime
            .host
            .open_popup(&namedrop::Popup {
                entrypoint: match mode {
                    UiMode::Name => "naming",
                    UiMode::Manage => "management",
                    UiMode::Doctor => "doctor",
                    UiMode::Status => "agent-status",
                    UiMode::Tour => tour::ENTRYPOINT,
                },
                width: if mode == UiMode::Status { "84" } else { "72" },
                height: if mode == UiMode::Name { "20" } else { "24" },
                env: &[
                    ("NAMEDROP_TARGET_WORKSPACE", workspace.to_owned()),
                    ("NAMEDROP_TARGET_PANE", pane.to_owned()),
                    ("NAMEDROP_TARGET_TERMINAL", terminal),
                    ("NAMEDROP_TARGET_SESSION", session),
                    (
                        "HERDR_PLUGIN_STATE_DIR",
                        runtime.state_directory.display().to_string(),
                    ),
                    ("HERDR_SOCKET_PATH", runtime.session_socket_id.to_owned()),
                ],
            })
            .map_err(String::from)
    })
}

fn target_pane(runtime: &ProcessRuntime<'_>) -> Result<DiscoveredPane, String> {
    let workspace = runtime
        .workspace_id
        .ok_or("This popup is missing its invoking space")?;
    runtime
        .host
        .space(namedrop::SpaceLocator::Workspace(workspace), None)?
        .panes
        .into_iter()
        .find(|p| Some(p.pane_id.as_str()) == runtime.current_pane_id)
        .ok_or_else(|| {
            "The invoking pane has closed. Open the popup again from your pane.".to_owned()
        })
}

fn target_terminal(runtime: &ProcessRuntime<'_>) -> Result<String, String> {
    target_pane(runtime).map(|pane| pane.terminal_id)
}

pub(super) fn run(options: &RuntimeOptions, mode: UiMode) -> ExitCode {
    if mode == UiMode::Status {
        return crate::agent_status_ui::run(options);
    }
    match run_prompt(options, mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Keep actionable errors visible instead of closing the popup immediately.
            eprintln!("\nNamedrop: {}", error.escape_debug());
            if io::stdin().is_terminal() {
                let _ = prompt_line("Enter or Esc closes: ");
            }
            ExitCode::from(65)
        }
    }
}

fn run_prompt(options: &RuntimeOptions, mode: UiMode) -> Result<(), String> {
    let claim = options.popup_target_claim()?;
    let workspace = &claim.workspace;
    let pane = &claim.pane;
    let terminal = &claim.terminal;
    with_runtime_options(options, |runtime| {
        let targeted = LifecycleRuntime {
            workspace_id: Some(workspace),
            current_pane_id: Some(pane),
            ..*runtime
        };
        let origin = target_pane(&targeted)?;
        if origin.terminal_id != *terminal {
            return Err(
                "The invoking pane changed. Close this popup and open it again.".to_owned(),
            );
        }
        let session = claim
            .resolved_session(options.host_kind() == Ok(HostKind::Hc), || {
                Ok(origin.agent_session_id.map(|id| id.as_str().to_owned()))
            })?
            .unwrap_or_default();
        match mode {
            UiMode::Name => name_prompt(&targeted, terminal, &options.environment).map(drop),
            UiMode::Manage => manage_prompt(&targeted, terminal, options),
            UiMode::Doctor => doctor_prompt(&targeted, terminal, &session, &options.environment),
            UiMode::Tour => tour_prompt(&targeted, terminal, &options.environment),
            UiMode::Status => unreachable!("status UI is dispatched before interactive prompts"),
        }
    })
}

fn doctor_prompt(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    session: &str,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    let _input_mode = InputMode::enter()?;
    let mut notice = String::new();
    'refresh: loop {
        let diagnosis = diagnose_runtime(runtime);
        let has_issues = diagnosis
            .issues
            .iter()
            .any(|issue| issue.severity != namedrop::HealthSeverity::Info);
        // A space with no names is where the tour helps most, and it always
        // has an issue to report too: no agent, or an agent without a name.
        let offer_tour = space_has_no_names(runtime);
        let report = namedrop::render_health_report_to_text(&diagnosis);
        // A refusal leads the page, wrapped and paged with the diagnostics so
        // it is never cut.
        let mut sources = Vec::new();
        if !notice.is_empty() {
            sources.extend([(Tone::Warn, notice.as_str()), (Tone::Plain, "")]);
        }
        let mut after_blank = false;
        for (index, line) in report.lines().enumerate() {
            sources.push((
                report_tone(index, line, after_blank, diagnosis.status),
                line,
            ));
            after_blank = line.is_empty();
        }
        if offer_tour {
            sources.extend([(Tone::Plain, ""), (Tone::Muted, TOUR_OFFER)]);
        }
        // Bound bytes as well as lines so wide text cannot push the controls
        // below the popup. Keep every character of diagnostics and commands.
        let lines: Vec<(Tone, String)> = sources
            .into_iter()
            .flat_map(|(tone, line)| wrap_exact(line, 68).into_iter().map(move |l| (tone, l)))
            .collect();
        let pages = lines.len().div_ceil(DOCTOR_PAGE_LINES).max(1);
        let mut page = 0;
        loop {
            // Herdr titles the popup "Namedrop Doctor", so the page opens on
            // the verdict instead of repeating that.
            clear_screen();
            let shown = lines.iter().skip(page * DOCTOR_PAGE_LINES);
            for (tone, line) in shown.take(DOCTOR_PAGE_LINES) {
                say(*tone, line);
            }
            println!();
            let mut controls = Vec::new();
            if pages > 1 {
                controls.push(format!("Page {}/{pages} · ←/→ Page", page + 1));
            }
            controls.push("R Refresh".to_owned());
            if has_issues {
                controls.push("A Ask agent to help".to_owned());
            }
            say(Tone::Keys, &controls.join(" · "));
            say(Tone::Keys, "Enter or Esc closes");
            // Read through the menu's key reader: an arrow key starts with the
            // Esc byte, and reading that byte alone closed the popup.
            match menu_key()? {
                MenuKey::Close | MenuKey::Enter | MenuKey::Char(b'q') => return Ok(()),
                MenuKey::Char(b'r') => {
                    notice.clear();
                    continue 'refresh;
                }
                // Naming from the tour changes what Doctor reports.
                MenuKey::Char(b't') if offer_tour => {
                    inline_tour(runtime, terminal, &mut notice, environment)?;
                    continue 'refresh;
                }
                // Only this key sends. Paging, refresh and close stay read-only.
                MenuKey::Char(b'a') if has_issues => {
                    if target_terminal(runtime)? != terminal {
                        return Err(
                            "The invoking pane changed. Nothing was sent; open Doctor again from your pane."
                                .to_owned(),
                        );
                    }
                    match namedrop::request_doctor_assistance(runtime, session) {
                        // The pane's agent may be unnamed, so name the pane
                        // rather than echoing a mention that may not exist.
                        Ok(_) => {
                            clear_screen();
                            say(
                                Tone::Plain,
                                "Herdr accepted the prompt. That is not the agent acting,\n\
                                 and no issue is fixed yet.\n\n\
                                 Go back to the pane you opened Doctor from for the agent's\n\
                                 report and any steps it needs from you.\n",
                            );
                            say(Tone::Keys, "Enter or Esc closes");
                            io::stdout().flush().map_err(|e| e.to_string())?;
                            let mut key = [0];
                            io::stdin().read(&mut key).map_err(|e| e.to_string())?;
                            return Ok(());
                        }
                        Err(error) => {
                            notice = error.escape_debug().to_string();
                            continue 'refresh;
                        }
                    }
                }
                MenuKey::Right | MenuKey::Down | MenuKey::Char(b'>') => {
                    page = (page + 1).min(pages - 1);
                }
                MenuKey::Left | MenuKey::Up | MenuKey::Char(b'<') => {
                    page = page.saturating_sub(1);
                }
                _ => {}
            }
        }
    }
}

/// Names the invoking pane and returns the confirmation it printed, or
/// `None` when the person cancelled.
fn name_prompt(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<Option<String>, String> {
    let default = namedrop::default_pane_type(runtime)?;
    say(Tone::Strong, "Name this pane\n");
    say(
        Tone::Muted,
        "Letters, digits and single hyphens. Esc cancels without saving.\n",
    );
    let Some(alias) = prompt_line("Mention name: @")?.filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    println!();
    say(
        Tone::Muted,
        "Terminal runs shell commands; Agent receives tasks.",
    );
    let kind = loop {
        let label = format!(
            "Pane type: terminal / agent{}: ",
            default
                .map(|p| format!(" [{}]", p.as_str()))
                .unwrap_or_default()
        );
        let Some(answer) = prompt_line(&label)? else {
            return Ok(None);
        };
        match answer.trim().to_ascii_lowercase().as_str() {
            "" if default.is_some() => break default.unwrap(),
            "terminal" | "t" => break PaneType::Terminal,
            "agent" | "a" => break PaneType::Agent,
            _ => say(Tone::Warn, "Choose terminal (t) or agent (a)."),
        }
    };
    // A named terminal runs shell commands and has no persona, so it is never
    // asked for one.
    let persona = if kind == PaneType::Agent {
        clear_screen();
        // Suggestions run on the invoking pane's own CLI and its existing login.
        let origin = target_pane(runtime)?;
        let saved = match namedrop::StateStore::open_read_only(runtime.state_directory)
            .map_err(|e| e.to_string())?
        {
            Some(mut store) => store
                .transaction(|tx| {
                    Ok(tx
                        .list_pane_addresses(&namedrop::StateNamespace::for_space(
                            runtime.session_socket_id,
                            &origin.workspace_id,
                        ))?
                        .into_iter()
                        .find(|a| a.pane_id.as_deref() == Some(origin.pane_id.as_str()))
                        .and_then(|a| a.persona))
                })
                .map_err(|e| e.to_string())?,
            None => None,
        };
        persona_editor(
            &alias,
            saved.as_deref(),
            &origin.agent.unwrap_or_default(),
            environment,
        )?
    } else {
        Persona::Keep
    };
    // Esc anywhere in naming saves nothing, and the persona step is part of
    // naming. Choosing Generic is a decision, not a cancellation, so it names
    // the pane like any other answer.
    if persona == Persona::Cancel {
        return Ok(None);
    }
    // The physical target is rechecked after every answer, including the one
    // the editor waited on, so nothing is saved against a replaced pane.
    if target_terminal(runtime)? != terminal {
        return Err("The invoking pane changed. Nothing was named; open naming again.".to_owned());
    }
    let mut input = serde_json::json!({"alias":alias,"pane_type":kind});
    if let Some(description) = persona.description() {
        input["persona"] = serde_json::json!(description);
    }
    let result = execute_lifecycle_operation("name", input, runtime)?;
    let named = format!(
        "Named {} as {}.",
        result["alias"].as_str().unwrap_or("pane"),
        kind.as_str()
    );
    println!();
    say(Tone::Good, &named);
    let advice = &result["codex_app_server"];
    if advice.is_object() {
        println!();
        show_app_server_advice(advice);
        println!();
        prompt_line("Enter or Esc closes: ")?;
    }
    Ok(Some(named))
}

/// Codex's shared app-server advice from a `name` result.
fn show_app_server_advice(advice: &serde_json::Value) {
    let text = |key: &str| advice[key].as_str().unwrap_or_default().to_owned();
    let steps = advice["steps"].as_array().into_iter().flatten();
    let lines = [(Tone::Warn, text("summary"))]
        .into_iter()
        .chain(["explanation", "fix"].map(|key| (Tone::Plain, text(key))))
        .chain(steps.filter_map(|step| step.as_str()).map(|step| {
            let tone = if step.starts_with("codex ") {
                Tone::Code
            } else {
                Tone::Plain
            };
            (tone, step.to_owned())
        }));
    for (tone, line) in lines {
        for wrapped in wrap_exact(&line, POPUP_COLUMNS) {
            say(tone, &wrapped);
        }
    }
}

/// The line Manage and Doctor add when the space has no names yet.
const TOUR_OFFER: &str = "Press T for the tour.";

/// The tour popup. Opening it counts as showing the current presentation, so
/// a tour someone opened by hand never pops up again on its own.
fn tour_prompt(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    let _input_mode = InputMode::enter()?;
    // A marker that cannot be written only means the tour may open again, so
    // it is reported on the page instead of costing the tour itself.
    let warning = tour::mark_shown(runtime.state_directory, tour::CURRENT_ID)
        .err()
        .map(|error| format!("The tour may open again: {}", error.escape_debug()));
    let presentation = tour::current()?;
    tour_pages(
        runtime,
        terminal,
        &presentation,
        warning.as_deref(),
        environment,
    )
}

/// Pages the current tour from Manage or Doctor, which stay open after it; a
/// tour that cannot load becomes their notice. Only the tour popup records
/// the tour as shown, so Doctor stays read-only.
fn inline_tour(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    notice: &mut String,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    match tour::current() {
        Ok(presentation) => tour_pages(runtime, terminal, &presentation, None, environment),
        Err(error) => {
            *notice = format!("Could not open the tour: {}", error.escape_debug());
            Ok(())
        }
    }
}

/// Pages a presentation the way Doctor pages its report, until Enter, Esc or
/// Q closes it. `warning` leads the first page. A page offering naming runs
/// the ordinary name prompt on N and then returns to that page, showing what
/// was named once at its top.
fn tour_pages(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    presentation: &Presentation,
    warning: Option<&str>,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    let count = presentation.pages.len();
    let mut index = 0;
    let mut named: Option<String> = None;
    while let Some(page) = presentation.pages.get(index) {
        clear_screen();
        let notice = named.take().map(|text| (Tone::Good, text)).or_else(|| {
            warning
                .filter(|_| index == 0)
                .map(|text| (Tone::Warn, text.to_owned()))
        });
        if let Some((tone, text)) = notice {
            for line in wrap_exact(&text, POPUP_COLUMNS) {
                say(tone, &line);
            }
            println!();
        }
        say(Tone::Strong, &page.title);
        println!();
        for line in tour::wrapped_body(page, tour::BODY_WIDTH) {
            say(Tone::Plain, &line);
        }
        println!();
        let offers_naming = page.action == Some(PageAction::Name);
        let mut controls = format!("Page {}/{count} · ←/→ Page", index + 1);
        if offers_naming {
            controls.push_str(" · N Name this pane");
        }
        say(Tone::Keys, &controls);
        say(Tone::Keys, "Enter or Esc closes");
        match menu_key()? {
            MenuKey::Close | MenuKey::Enter | MenuKey::Char(b'q') => return Ok(()),
            MenuKey::Right | MenuKey::Down | MenuKey::Char(b'>' | b' ') => {
                index = (index + 1).min(count - 1);
            }
            MenuKey::Left | MenuKey::Up | MenuKey::Char(b'<') => {
                index = index.saturating_sub(1);
            }
            MenuKey::Char(b'n') if offers_naming => {
                clear_screen();
                named = name_prompt(runtime, terminal, environment)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether this space has no names at all. It reads without creating or
/// changing state, because Doctor stays read-only; a store that cannot be
/// read answers `false`, so the tour is offered only where it is known to fit.
fn space_has_no_names(runtime: &ProcessRuntime<'_>) -> bool {
    let Some(workspace) = runtime.workspace_id else {
        return false;
    };
    match namedrop::StateStore::open_read_only(runtime.state_directory) {
        Ok(None) => true,
        Ok(Some(mut store)) => store
            .transaction(|tx| {
                tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                    runtime.session_socket_id,
                    workspace,
                ))
            })
            .is_ok_and(|addresses| addresses.is_empty()),
        Err(_) => false,
    }
}

fn manage_prompt(
    runtime: &ProcessRuntime<'_>,
    terminal: &str,
    options: &RuntimeOptions,
) -> Result<(), String> {
    let environment = &options.environment;
    let _input_mode = InputMode::enter()?;
    let mut selected = 0usize;
    let mut notice = String::new();
    loop {
        let listing = execute_lifecycle_operation("address.list", serde_json::json!({}), runtime)
            .and_then(|data| {
                serde_json::from_value::<Vec<PaneAddressRecord>>(data).map_err(|e| e.to_string())
            });
        let (addresses, load_error) = match listing {
            Ok(addresses) => (addresses, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let offer_tour = load_error.is_none() && addresses.is_empty();
        let pages = addresses.len().div_ceil(8).max(1);
        selected = selected.min(addresses.len().saturating_sub(1));
        let page = selected / 8;
        clear_screen();
        say(Tone::Strong, "Manage names\n");
        if !notice.is_empty() {
            say(Tone::Plain, &format!("{notice}\n"));
        }
        if let Some(error) = load_error {
            let error = format!("Could not list names: {}", error.escape_debug());
            say(Tone::Warn, &error);
        } else if addresses.is_empty() {
            say(
                Tone::Muted,
                "No named panes yet. Press N to name this pane.",
            );
            say(Tone::Muted, TOUR_OFFER);
        }
        for (index, address) in addresses.iter().enumerate().skip(page * 8).take(8) {
            let location = address
                .pane_label
                .as_deref()
                .or(address.pane_id.as_deref())
                .unwrap_or("No pane");
            let status = if address.collaboration_disabled {
                "delivery off"
            } else {
                address
                    .agent_status
                    .as_deref()
                    .unwrap_or(address.pane_type.as_str())
            };
            let name = format!("@{:<20}", menu_text(&address.alias, 20));
            // The selection highlights the fixed-width name cell, so the bar
            // is the same length on every row.
            let marked = if index == selected {
                (Tone::Selected, format!("> {name}"))
            } else {
                (Tone::Name, format!("  {name}"))
            };
            let kind = format!("{:<8}", address.pane_type.as_str());
            let state = format!("{:<12}", menu_text(status, 12));
            println!(
                "{}",
                dress(&[
                    (marked.0, &marked.1),
                    (Tone::Plain, "  "),
                    (Tone::Muted, &kind),
                    (Tone::Plain, "  "),
                    (status_tone(status), &state),
                    (Tone::Plain, "  "),
                    (Tone::Muted, &menu_text(location, 16)),
                ])
            );
        }
        if pages > 1 {
            println!();
            let paging = format!("Page {}/{} · ←/→ Page", page + 1, pages);
            say(Tone::Keys, &paging);
        }
        // What the selected agent is for, kept off the status column so the
        // two facts are never read as one.
        if let Some(address) = addresses.get(selected) {
            println!();
            say(Tone::Muted, &format!("Persona: {}", persona_text(address)));
        }
        println!();
        say(Tone::Keys, "N Name this pane · R Refresh · Esc Close");
        if !addresses.is_empty() {
            say(Tone::Keys, "↑/↓ Select · Enter Manage");
        }
        match menu_key()? {
            MenuKey::Close => return Ok(()),
            MenuKey::Char(b'n') => {
                notice.clear();
                clear_screen();
                name_prompt(runtime, terminal, environment)?;
                continue;
            }
            MenuKey::Down => {
                selected = (selected + 1).min(addresses.len().saturating_sub(1));
                continue;
            }
            MenuKey::Up => {
                selected = selected.saturating_sub(1);
                continue;
            }
            MenuKey::Right | MenuKey::Char(b'>') => {
                selected = ((page + 1) * 8).min(addresses.len().saturating_sub(1));
                continue;
            }
            MenuKey::Left | MenuKey::Char(b'<') => {
                selected = page.saturating_sub(1) * 8;
                continue;
            }
            MenuKey::Char(b'r') => {
                notice.clear();
                continue;
            }
            MenuKey::Char(b't') if offer_tour => {
                notice.clear();
                inline_tour(runtime, terminal, &mut notice, environment)?;
                continue;
            }
            MenuKey::Enter => {}
            _ => continue,
        }
        let Some(address) = addresses.get(selected) else {
            continue;
        };
        let is_agent = address.pane_type == PaneType::Agent;
        clear_screen();
        say(Tone::Name, &format!("@{}\n", address.alias));
        say(
            Tone::Muted,
            &format!("Persona: {}\n", persona_text(address)),
        );
        say(
            Tone::Menu,
            "R Rename\nB Attach to this pane\nU Detach from pane",
        );
        if is_agent {
            say(Tone::Menu, "P Edit persona");
        }
        say(
            Tone::Menu,
            &format!(
                "{}\nX Remove name\nEnter/Esc Back\n",
                if address.collaboration_disabled {
                    "E Enable delivery"
                } else {
                    "D Disable delivery"
                }
            ),
        );
        let MenuKey::Char(action) = menu_key()? else {
            continue;
        };
        let mut input = serde_json::json!({"address_id": address.id});
        // A second operation the same action asks for, run after the first one
        // succeeds. Renaming is the only action that has one: it offers a
        // replacement persona without making the rename depend on it.
        let mut follow_up: Option<serde_json::Value> = None;
        let (operation, confirmation) = match action {
            b'r' => {
                let Some(alias) = prompt_line("New name: @")?.filter(|s| !s.trim().is_empty())
                else {
                    continue;
                };
                let alias = alias
                    .trim()
                    .strip_prefix('@')
                    .unwrap_or(alias.trim())
                    .to_owned();
                input["alias"] = serde_json::json!(alias);
                if is_agent {
                    // The saved persona stays until a replacement is accepted
                    // here, so a rename alone never redefines the agent.
                    clear_screen();
                    let host = target_pane(runtime)?.agent.unwrap_or_default();
                    follow_up =
                        persona_editor(&alias, address.persona.as_deref(), &host, environment)?
                            .description()
                            .map(|description| {
                                serde_json::json!({
                                    "address_id": address.id,
                                    "description": description,
                                })
                            });
                }
                ("address.rename", None)
            }
            b'p' if is_agent => {
                clear_screen();
                let host = target_pane(runtime)?.agent.unwrap_or_default();
                let choice = persona_editor(
                    &address.alias,
                    address.persona.as_deref(),
                    &host,
                    environment,
                )?;
                let Some(description) = choice.description() else {
                    notice = if choice == Persona::Cancel {
                        "Cancelled. The saved persona is unchanged.".into()
                    } else {
                        "The persona is unchanged.".into()
                    };
                    continue;
                };
                input["description"] = serde_json::json!(description);
                ("address.persona", None)
            }
            b'b' => {
                input["pane_id"] = serde_json::json!(runtime.current_pane_id);
                (
                    "address.bind",
                    Some("Attach this name to the pane that opened the manager?"),
                )
            }
            b'u' => (
                "address.unbind",
                Some("Detach this name? Its pane and agent will stay open."),
            ),
            b'x' => (
                "address.remove",
                Some(
                    "Remove this name and its reserved aliases?\nHistory is kept; its pane and agent stay open.",
                ),
            ),
            b'e' if address.collaboration_disabled => ("address.enable", None),
            b'd' if !address.collaboration_disabled => ("address.disable", None),
            _ => {
                notice = "Choose one of the displayed actions.".into();
                continue;
            }
        };
        if let Some(question) = confirmation
            && !prompt_line(&format!("{question} [y/N]: "))?
                .is_some_and(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
        {
            notice = "Cancelled. Nothing changed.".into();
            continue;
        }
        if target_terminal(runtime)? != terminal {
            return Err(
                "The invoking pane changed. Nothing changed; open the manager again.".into(),
            );
        }
        let latest: Vec<PaneAddressRecord> = serde_json::from_value(execute_lifecycle_operation(
            "address.list",
            serde_json::json!({}),
            runtime,
        )?)
        .map_err(|e| e.to_string())?;
        if !latest.iter().any(|current| {
            current.id == address.id
                && current.alias == address.alias
                && current.pane_id == address.pane_id
                && current.pane_type == address.pane_type
                && current.persona == address.persona
                && current.collaboration_disabled == address.collaboration_disabled
        }) {
            notice = "This name changed while the menu was open. Select it again.".into();
            continue;
        }
        let result = if is_control_operation(operation) {
            execute_control_operation(operation, input, runtime)
        } else {
            execute_lifecycle_operation(operation, input, runtime)
        };
        // The rename has already happened by now, so a persona that could not be
        // saved is reported beside it rather than in place of it.
        let persona_error = match (&result, follow_up) {
            (Ok(_), Some(persona)) => {
                execute_lifecycle_operation("address.persona", persona, runtime).err()
            }
            _ => None,
        };
        notice = match result {
            Ok(data) => {
                let warning = if needs_mailbox_watcher(&data) {
                    wake_runtime_mailboxes(runtime, options).err()
                } else {
                    None
                };
                let mut message =
                    render_command_result_to_text(&CommandResult::success(operation, data));
                if let Some(error) = warning {
                    message.push_str(&format!(
                        "\nCommand queued; could not start shell watcher: {}",
                        error.escape_debug()
                    ));
                }
                message
            }
            Err(error) => format!(
                "Could not change @{}: {}",
                address.alias,
                error.escape_debug()
            ),
        };
        if let Some(error) = persona_error {
            notice.push_str(&format!(
                "\nThe persona was not saved: {}",
                error.escape_debug()
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Persona editor
// ---------------------------------------------------------------------------

/// What the user decided about a persona.
///
/// `Cancel` is backing out, and it is not the same answer as `Keep`: naming a
/// pane promises that Esc saves nothing, so a cancelled persona step abandons
/// the naming it was part of. `Keep` is an explicit Save of text the user did
/// not change, which is also how a migrated description too long for today's
/// bound survives being looked at.
#[derive(Debug, PartialEq)]
enum Persona {
    Cancel,
    Keep,
    Generic,
    Set(String),
}

impl Persona {
    /// The text a `name` or `address.persona` operation carries. Blank is how
    /// Generic is chosen, so clearing and an emptied field are the same input.
    /// `None` sends no field at all, which keeps whatever is saved; cancelling
    /// and saving unchanged text both write nothing.
    fn description(&self) -> Option<String> {
        match self {
            Self::Cancel | Self::Keep => None,
            Self::Generic => Some(String::new()),
            Self::Set(text) => Some(text.clone()),
        }
    }
}

/// Cancels every suggestion still running. Declared inside the thread scope, so
/// leaving the editor reaps its worker rather than waiting the provider out.
#[derive(Default)]
struct Workers(RefCell<Vec<Arc<AtomicBool>>>);

impl Workers {
    fn start(&self) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.0.borrow_mut().push(cancelled.clone());
        cancelled
    }

    fn cancel(&self) {
        for cancelled in self.0.borrow().iter() {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The editor's own state. The draft is bytes because a character arrives one
/// byte at a time, so nothing here can split one; UTF-8 is checked once, where
/// the text leaves for an operation.
struct Editor {
    draft: Vec<u8>,
    saved: Vec<u8>,
    /// The text field has focus. Letters type instead of choosing a menu key.
    editing: bool,
    /// What the field held when editing began, so backing out of an edit is not
    /// a way to lose the persona that was already saved.
    before_edit: Vec<u8>,
    suggestion: Option<Result<String, String>>,
    working: bool,
    started: Instant,
    notice: String,
}

/// Whether a byte begins a character rather than continuing one. UTF-8
/// continuation bytes are the only ones of the form `10xxxxxx`.
fn starts_character(byte: u8) -> bool {
    byte & 0xc0 != 0x80
}

impl Editor {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.draft).into_owned()
    }

    /// Whether the draft ends part-way through a character: valid UTF-8 up to a
    /// final sequence that is unfinished rather than wrong. That is the only
    /// state in which a continuation byte belongs, so a byte whose lead was
    /// refused has nothing to continue and is refused with it.
    fn pending_character(&self) -> bool {
        std::str::from_utf8(&self.draft).is_err_and(|error| error.error_len().is_none())
    }

    /// Characters in the draft, counted from the bytes that start one. This is
    /// what the limit is measured in, so a multi-byte character costs one
    /// rather than one per byte or one replacement character per stray byte.
    fn characters(&self) -> usize {
        self.draft
            .iter()
            .copied()
            .filter(|byte| starts_character(*byte))
            .count()
    }

    /// What Enter commits. Text the user never touched is left alone rather
    /// than rewritten, so a description migrated from an older Namedrop is not
    /// re-validated against today's bound just because it was displayed.
    fn save(&self) -> Result<Persona, String> {
        if self.draft == self.saved {
            return Ok(Persona::Keep);
        }
        let text = String::from_utf8(self.draft.clone())
            .map_err(|_| "That text is not valid UTF-8. Edit it and try again.".to_owned())?;
        Ok(if text.trim().is_empty() {
            Persona::Generic
        } else {
            Persona::Set(text.trim().to_owned())
        })
    }

    /// One block of persona text, bounded so neither a long draft nor a long
    /// suggestion can push the controls out of the popup.
    fn block(&self, text: &str) -> Vec<String> {
        let mut lines = wrap(&sanitize(text), POPUP_COLUMNS - 2);
        if lines.len() > MAX_PERSONA_LINES {
            lines.truncate(MAX_PERSONA_LINES);
            lines[MAX_PERSONA_LINES - 1].push('…');
        }
        lines.into_iter().map(|line| format!("  {line}")).collect()
    }

    fn frame(&self, alias: &str) -> Vec<(Tone, String)> {
        let blank = || (Tone::Plain, String::new());
        let mut lines = vec![
            (
                Tone::Strong,
                format!("Persona for @{}", menu_text(alias, 40)),
            ),
            blank(),
            (Tone::Strong, "Persona".to_owned()),
        ];
        if self.draft.is_empty() && !self.editing {
            lines.push((
                Tone::Muted,
                "  Generic. No specialization; still takes ordinary work.".to_owned(),
            ));
        } else {
            let mut block = self.block(&self.text());
            // The terminal cursor is hidden, so typing shows where it lands.
            if self.editing
                && let Some(last) = block.last_mut()
            {
                last.push('▌');
            }
            lines.extend(block.into_iter().map(|line| (Tone::Plain, line)));
        }
        lines.push(blank());
        lines.push(match &self.suggestion {
            _ if self.working => (
                Tone::Muted,
                format!("Suggesting… {}s", self.started.elapsed().as_secs()),
            ),
            Some(Ok(_)) => (Tone::Strong, "Suggestion".to_owned()),
            Some(Err(_)) => (Tone::Strong, "No suggestion".to_owned()),
            None => (Tone::Muted, "No suggestion asked for".to_owned()),
        });
        // Why there is none is wrapped like the suggestion itself, so a
        // provider's own wording is readable rather than cut to a column.
        match &self.suggestion {
            Some(Ok(suggestion)) => {
                let block = self.block(suggestion);
                lines.extend(block.into_iter().map(|line| (Tone::Plain, line)));
            }
            Some(Err(error)) => {
                let block = self.block(error);
                lines.extend(block.into_iter().map(|line| (Tone::Warn, line)));
            }
            None => {}
        }
        if !self.notice.is_empty() {
            lines.push(blank());
            let notice = wrap(&sanitize(&self.notice), POPUP_COLUMNS);
            lines.extend(notice.into_iter().map(|line| (Tone::Warn, line)));
        }
        lines.push(blank());
        let hints: &[&str] = if self.editing {
            &["Typing · Enter Done · Esc Discard this edit"]
        } else {
            &[
                "Enter Save · E Edit text · G Generic",
                "A Use suggestion · S Suggest again · Esc Cancel",
            ]
        };
        lines.extend(hints.iter().map(|hint| (Tone::Keys, (*hint).to_owned())));
        lines
    }
}

/// Edits one agent's persona, asking an isolated worker for a suggestion in the
/// background while the user reads and types.
///
/// The suggestion is only ever a draft: it is shown beside the text, never
/// written into it, and nothing is saved until the user presses Save or
/// Generic. Saving, editing and Generic all work while a suggestion is still
/// running or after it failed, so inference can never block the decision.
///
/// `saved` is the persona already stored; `host` is the invoking pane's own
/// CLI, and an unsupported one simply reports that it has no suggestions.
fn persona_editor(
    alias: &str,
    saved: Option<&str>,
    host: &str,
    environment: &namedrop::environment::EnvironmentSnapshot,
) -> Result<Persona, String> {
    let interactive = io::stdin().is_terminal();
    let _mode = InputMode::polling()?;
    let draft = saved.unwrap_or_default().as_bytes().to_vec();
    let mut editor = Editor {
        saved: draft.clone(),
        draft,
        editing: false,
        before_edit: Vec::new(),
        suggestion: None,
        working: true,
        started: Instant::now(),
        notice: String::new(),
    };
    thread::scope(|scope| -> Result<Persona, String> {
        let workers = Workers::default();
        let (sender, receiver) = mpsc::channel::<(u64, Result<String, String>)>();
        let mut generation = 0u64;
        let ask = |generation: u64, draft: String, sender: mpsc::Sender<_>| {
            let cancelled = workers.start();
            let alias = alias.to_owned();
            let host = host.to_owned();
            scope.spawn(move || {
                let suggestion = namedrop::suggest_agent_persona_with_environment(
                    &alias,
                    &draft,
                    &host,
                    &cancelled,
                    environment,
                );
                let _ = sender.send((generation, suggestion));
            });
        };
        // One suggestion as the editor opens, and afterwards only when asked
        // for: a suggestion per keystroke is neither wanted nor paid for.
        ask(generation, editor.text(), sender.clone());
        let mut shown = Vec::new();
        loop {
            // An answer to superseded text describes a draft the editor has
            // already moved past, so it is dropped rather than displayed.
            while let Ok((answered, suggestion)) = receiver.try_recv() {
                if answered == generation {
                    editor.suggestion = Some(suggestion);
                    editor.working = false;
                }
            }
            // Without a terminal there is no concurrent typist, so settle the
            // suggestion in hand before the next scripted key is applied.
            while !interactive && editor.working {
                match receiver.recv_timeout(SUGGESTION_WAIT) {
                    Ok((answered, suggestion)) if answered == generation => {
                        editor.suggestion = Some(suggestion);
                        editor.working = false;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            let frame = editor.frame(alias);
            if frame != shown {
                clear_screen();
                for (tone, line) in &frame {
                    say(*tone, line);
                }
                io::stdout().flush().map_err(|e| e.to_string())?;
                shown = frame;
            }
            let key = editor_key(interactive)?;
            if key != EditorKey::Idle {
                editor.notice.clear();
            }
            match key {
                EditorKey::Idle | EditorKey::Other => {}
                EditorKey::Cancel if editor.editing => {
                    editor.draft = std::mem::take(&mut editor.before_edit);
                    editor.editing = false;
                }
                EditorKey::Cancel => return Ok(Persona::Cancel),
                EditorKey::Enter if editor.editing => editor.editing = false,
                EditorKey::Enter => match editor.save() {
                    Ok(persona) => return Ok(persona),
                    Err(error) => editor.notice = error,
                },
                EditorKey::Backspace if editor.editing => {
                    // Back over the whole character, continuation bytes first.
                    while editor
                        .draft
                        .pop()
                        .is_some_and(|byte| !starts_character(byte))
                    {}
                }
                EditorKey::Backspace => {}
                EditorKey::Char(byte) if editor.editing => {
                    // A character arrives one byte at a time. The limit is
                    // applied to the byte that starts one, and a continuation
                    // byte is taken only when a character is actually under
                    // way. So an over-limit character is refused whole, rather
                    // than leaving half of one behind to make the draft the
                    // user did type unsaveable.
                    let accepted = if starts_character(byte) {
                        editor.characters() < MAX_PERSONA_CHARS
                    } else {
                        editor.pending_character()
                    };
                    if accepted {
                        editor.draft.push(byte);
                    } else {
                        editor.notice =
                            format!("A persona is at most {MAX_PERSONA_CHARS} characters.");
                    }
                }
                EditorKey::Char(byte) => match byte.to_ascii_lowercase() {
                    b'e' => {
                        editor.before_edit = editor.draft.clone();
                        editor.editing = true;
                    }
                    b'g' => return Ok(Persona::Generic),
                    b'a' => match &editor.suggestion {
                        Some(Ok(suggestion)) => editor.draft = suggestion.clone().into_bytes(),
                        _ => editor.notice = "No suggestion to use yet.".to_owned(),
                    },
                    b's' => {
                        // Superseding cancels and reaps the worker in flight
                        // before another one is paid for.
                        workers.cancel();
                        generation += 1;
                        editor.suggestion = None;
                        editor.working = true;
                        editor.started = Instant::now();
                        ask(generation, editor.text(), sender.clone());
                    }
                    _ => {}
                },
            }
        }
    })
}

#[derive(Debug, PartialEq)]
enum EditorKey {
    Char(u8),
    Enter,
    Backspace,
    Cancel,
    /// The poll expired with no key pressed.
    Idle,
    Other,
}

/// One key from the polled popup terminal. Control bytes never reach the draft:
/// the text field takes printable bytes only, and the saved text is validated
/// again by the operation that stores it.
fn editor_key(interactive: bool) -> Result<EditorKey, String> {
    let mut byte = [0];
    if io::stdin().read(&mut byte).map_err(|e| e.to_string())? == 0 {
        return Ok(if interactive {
            EditorKey::Idle
        } else {
            EditorKey::Cancel
        });
    }
    Ok(match byte[0] {
        b'\n' | b'\r' => EditorKey::Enter,
        0x08 | 0x7f => EditorKey::Backspace,
        0x04 => EditorKey::Cancel,
        0x1b => {
            // Arrow keys arrive as an escape sequence. The poll returns no
            // further byte for a bare Esc rather than waiting for one.
            if io::stdin().read(&mut byte).map_err(|e| e.to_string())? == 0 {
                return Ok(EditorKey::Cancel);
            }
            if !matches!(byte[0], b'[' | b'O') {
                return Ok(EditorKey::Other);
            }
            let mut length = 0;
            while io::stdin().read(&mut byte).map_err(|e| e.to_string())? != 0 {
                length += 1;
                if (0x40..=0x7e).contains(&byte[0]) || length == 16 {
                    break;
                }
            }
            EditorKey::Other
        }
        byte if !byte.is_ascii_control() => EditorKey::Char(byte),
        _ => EditorKey::Other,
    })
}

/// The persona the popup shows for one name, Generic spelled out. Saved text is
/// displayed safely without being rewritten: a description migrated from an
/// older Namedrop may be longer than today's bound or carry control bytes.
fn persona_text(address: &PaneAddressRecord) -> String {
    // Retyping a named agent to a terminal keeps its persona stored but inert,
    // because only explicit user direction changes one. A terminal has no
    // persona to show, so the text stays saved and out of sight.
    if address.pane_type != PaneType::Agent {
        return "—".to_owned();
    }
    match address.persona.as_deref() {
        Some(persona) => {
            wrap(&sanitize(persona), POPUP_COLUMNS - "Persona: ".len()).join("\n         ")
        }
        None => "Generic".to_owned(),
    }
}

fn clear_screen() {
    if io::stdout().is_terminal() {
        // Hidden while a page is only read; `prompt_line` shows it for typing.
        print!("\x1b[?25l\x1b[2J\x1b[H");
    }
}

/// Prints popup text, one tone for every line of it.
fn say(tone: Tone, text: &str) {
    for line in text.split('\n') {
        println!("{}", dress(&[(tone, line)]));
    }
}

/// One popup line of coloured spans. On a real terminal it gets a one-cell
/// gutter, so text never touches the popup's border, and colour unless
/// NO_COLOR is set. Piped output, which tests and logs read, stays the plain
/// text exactly.
fn dress(spans: &[(Tone, &str)]) -> String {
    let terminal = io::stdout().is_terminal();
    let color = terminal && env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let text: String = spans
        .iter()
        .map(|&(tone, text)| {
            if color {
                tint(tone, text)
            } else {
                text.to_owned()
            }
        })
        .collect();
    if terminal { format!(" {text}") } else { text }
}

/// How the doctor colours one report line: the verdict by the report's own
/// status, each issue's summary (it follows a blank line) in bold, and the
/// labelled lines by how the report writes them.
fn report_tone(index: usize, line: &str, after_blank: bool, status: HealthStatus) -> Tone {
    if index == 0 {
        return match status {
            HealthStatus::Healthy => Tone::Good,
            HealthStatus::NeedsAttention => Tone::Warn,
            HealthStatus::Unavailable => Tone::Bad,
        };
    }
    if line.starts_with("Command: ") {
        Tone::Code
    } else if line.starts_with("Target: ") {
        Tone::Muted
    } else if after_blank {
        Tone::Strong
    } else {
        Tone::Plain
    }
}

/// The colour of a name's status in the manager. Anything that is not work
/// or a problem, including the pane type shown when no agent reports a
/// status, stays quiet.
fn status_tone(status: &str) -> Tone {
    match status {
        "working" | "busy" | "running" => Tone::Good,
        "blocked" | "attention" | "delivery off" => Tone::Warn,
        _ => Tone::Muted,
    }
}

/// Wraps to `limit` bytes without dropping or changing a character, since
/// diagnostics and commands are copied from here. A line breaks after its last
/// space that fits, and mid-word only when a word alone is too long.
fn wrap_exact(line: &str, limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = line;
    while rest.len() > limit {
        let mut end = limit;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(space) = rest[..end].rfind(' ').filter(|&space| space > 0) {
            end = space + 1;
        }
        lines.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    lines.push(rest.to_owned());
    lines
}

fn menu_text(value: &str, limit: usize) -> String {
    let safe = value.escape_debug().to_string();
    if safe.len() <= limit {
        return safe;
    }
    // A byte limit also bounds displayed columns for UTF-8 labels.
    let mut end = limit - 3;
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &safe[..end])
}

#[derive(Debug, PartialEq)]
enum MenuKey {
    Char(u8),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Close,
    Other,
}

fn menu_key() -> Result<MenuKey, String> {
    io::stdout().flush().map_err(|e| e.to_string())?;
    let _mode = InputMode::enter()?;
    let mut byte = [0];
    if io::stdin().read(&mut byte).map_err(|e| e.to_string())? == 0 {
        return Ok(MenuKey::Close);
    }
    Ok(match byte[0] {
        b'\n' | b'\r' => MenuKey::Enter,
        0x04 => MenuKey::Close,
        0x1b => {
            // Arrow keys start with Esc. Allow 100 ms for the rest, while a
            // standalone Esc closes without waiting for another key.
            if io::stdin().is_terminal()
                && !Command::new("/bin/stty")
                    .args(["min", "0", "time", "1"])
                    .status()
                    .map_err(|e| e.to_string())?
                    .success()
            {
                return Err("Could not read menu keys".to_owned());
            }
            if io::stdin().read(&mut byte).map_err(|e| e.to_string())? == 0 {
                return Ok(MenuKey::Close);
            }
            if !matches!(byte[0], b'[' | b'O') {
                return Ok(MenuKey::Other);
            }
            let mut sequence = Vec::new();
            while io::stdin().read(&mut byte).map_err(|e| e.to_string())? != 0 {
                sequence.push(byte[0]);
                if (0x40..=0x7e).contains(&byte[0]) {
                    break;
                }
                if sequence.len() == 16 {
                    return Ok(MenuKey::Other);
                }
            }
            match sequence.as_slice() {
                b"A" => MenuKey::Up,
                b"B" => MenuKey::Down,
                b"C" => MenuKey::Right,
                b"D" => MenuKey::Left,
                _ => MenuKey::Other,
            }
        }
        key => MenuKey::Char(key.to_ascii_lowercase()),
    })
}

fn prompt_line(label: &str) -> Result<Option<String>, String> {
    if io::stdout().is_terminal() {
        print!("\x1b[?25h");
    }
    let lines: Vec<_> = label
        .split('\n')
        .map(|line| dress(&[(Tone::Plain, line)]))
        .collect();
    print!("{}", lines.join("\n"));
    io::stdout().flush().map_err(|e| e.to_string())?;
    let _mode = InputMode::enter()?;
    let interactive = io::stdin().is_terminal();
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        if io::stdin().read(&mut byte).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        match byte[0] {
            b'\n' | b'\r' => {
                println!();
                return String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|e| e.to_string());
            }
            0x1b | 0x04 => {
                println!();
                return Ok(None);
            }
            0x08 | 0x7f => {
                if !bytes.is_empty() {
                    let mut start = bytes.len() - 1;
                    while start > 0 && bytes[start] & 0xc0 == 0x80 {
                        start -= 1;
                    }
                    bytes.truncate(start);
                    if interactive {
                        print!("\u{8} \u{8}");
                    }
                }
            }
            b if !b.is_ascii_control() => {
                bytes.push(b);
                if interactive {
                    io::stdout().write_all(&byte).map_err(|e| e.to_string())?;
                }
            }
            _ => (),
        }
        io::stdout().flush().map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::wrap_exact;

    #[test]
    fn doctor_wrapping_breaks_at_spaces_and_keeps_every_character() {
        let line = "Command: 'namedrop' 'address' 'bind' --input-json '{\"alias\":\"w1\"}' done";
        let wrapped = wrap_exact(line, 20);
        assert_eq!(wrapped.concat(), line);
        assert!(wrapped.iter().all(|piece| piece.len() <= 20), "{wrapped:?}");
        // Words stay whole where a space fits; only an overlong word is cut.
        assert_eq!(wrapped[0], "Command: 'namedrop' ");
        let long = format!("x {}", "é".repeat(30));
        let wrapped = wrap_exact(&long, 9);
        assert_eq!(wrapped.concat(), long);
        assert!(wrapped.iter().all(|piece| piece.len() <= 9), "{wrapped:?}");
        assert_eq!(wrap_exact("", 9), [""]);
    }
}
