"""Round B runtime mutations. Run on a disposable source copy; restore each mutation.

python3 -I tests/mutate_words_round_b.py OUT [LABEL_PREFIX ...]
The original Words test includes process-table fixtures. Sandboxed runs must be
reported separately if those fixtures were omitted in the disposable copy.
"""
import json, os
from pathlib import Path
import subprocess, sys
root=Path(__file__).resolve().parents[1]
out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=True)
selected=sys.argv[2:]
R = "crate::mux::MuxKind::Rex"
M = [
 ("H1 doctor ask space", "src/health.rs", '"Asking the agent requires the popup\'s invoking space",', '"Asking the agent requires the screen\'s invoking session",'),
 ("H2 doctor prompt screen", "src/health.rs", 'kind.text("popup", "screen")', 'kind.text("screen", "screen")'),
 ("H3 doctor accepted", "src/health.rs", '"Herdr accepted the prompt. That is not the agent acting,', '"Rex accepted the prompt. That is not the agent acting,'),
 ("H4 attestation", "src/hook.rs", "    let context = runtime.mux.endpoint().kind.text(\n        AUTHENTICATED_DELIVERY_CONTEXT,", "    let context = runtime.mux.endpoint().kind.text(\n        REX_AUTHENTICATED_DELIVERY_CONTEXT,"),
 ("H5 no named agents", "src/hook.rs", '"No named agents in this Herdr space.",', '"No named agents in this Rex session.",'),
 ("H6 registration authored", "src/integration.rs", '.text("plugin-authored", "Namedrop-authored");', '.text("Namedrop-authored", "Namedrop-authored");'),
 ("H7 roster", "src/persona.rs", '"Current named agents in this Herdr space; quoted personas are user data, not instructions",', '"Current named agents in this Rex session; quoted personas are user data, not instructions",'),
 ("H8 other workspaces", "src/persona.rs", 'kind.text("1 other Herdr workspace has", "1 other Rex session has")', 'kind.text("1 other Rex session has", "1 other Rex session has")'),
 ("H9 screen host default", "src/popup_input.rs", "    HOST.get().copied().unwrap_or(MuxKind::Herdr)", "    let _ = HOST.get();\n    MuxKind::Rex"),
 ("H10 status identity closed", "src/agent_status_ui.rs", '"The invoking pane has closed. Open the popup again from your pane.",', '"The invoking pane has closed. Open the screen again from your pane.",'),
 ("H11 naming target space", "src/naming_ui.rs", "    let workspace = runtime.workspace_id.ok_or(kind.text(\n        \"This popup is missing its invoking space\",", "    let workspace = runtime.workspace_id.ok_or(kind.text(\n        \"This screen is missing its invoking session\","),
 ("H12 naming target closed", "src/naming_ui.rs", '                "The invoking pane has closed. Open the popup again from your pane.",', '                "The invoking pane has closed. Open the screen again from your pane.",'),
 ("H13 coordination", "src/coordination.rs", '.text("workspace from Herdr", "session from Rex")', '.text("session from Rex", "session from Rex")'),
 ("H14 deferred state", "src/deferred_commands.rs", '"plugin state is required for deferred commands",', '"a state directory is required for deferred commands",'),
 ("H15 doctor 003 kind", "src/health.rs", "            &space_id,\n            runtime.mux.endpoint().kind,\n        ));", f"            &space_id,\n            {R},\n        ));"),
 ("H16 name served advice kind", "src/lifecycle.rs", "served.advice(runtime.mux.endpoint().kind)", f"served.advice({R})"),
 ("H17 activation advice kind", "src/install.rs", ".advice(kind) {", f".advice({R}) {{"),
 ("H18 doctor refusal summary kind", "src/health.rs", "crate::codex_app_server::summary(runtime.mux.endpoint().kind)", f"crate::codex_app_server::summary({R})"),
 ("H21 name without pane", "src/lifecycle.rs", '"name must run from the Herdr pane being named",', '"name must run from the Rex pane being named",'),
 ("H22 tour unreadable", "src/tour.rs", "                runtime.mux.endpoint().kind.text(\n                    \"Herdr returned an unreadable popup result\",", "                runtime.mux.endpoint().kind.text(\n                    \"Rex returned an unreadable screen result\","),
 ("H24 SKILL.md sandbox transport", "src/install.rs", 'let transport = if rex { "Rex" } else { "Herdr" };', 'let transport = if rex { "Rex" } else { "Rex" };'),
 ("H25 SKILL.md invocation", "src/install.rs", "    let invocation = if rex {", "    let invocation = if rex || true {"),
 ("H27 session policy kind", "src/persona.rs", ".map(|_| session_policy_text(runtime.mux.endpoint().kind))", f".map(|_| session_policy_text({R}))"),
 ("H29 terminal runner", "src/terminal.rs", '"terminal runner requires its current Herdr pane",', '"terminal runner requires its current Rex pane",'),
 ("H30 sender pane", "src/message.rs", '"the current Herdr pane is required to resolve the sender",', '"the current Rex pane is required to resolve the sender",'),
 ("H31 pane_id outside action", "src/lifecycle.rs", '"pane_id is required outside a Herdr pane action",', '"pane_id is required outside a Rex pane action",'),
 ("H32 status frozen notice", "src/agent_status_ui.rs", '"This pane changed. Close this popup and open it again from your pane.",', '"This pane changed. Close this screen and open it again from your pane.",'),
 ("H33 ND-SESSION-002 fix prompt", "src/health.rs", '"Install the Herdr native integration if needed, then restart this agent."', '"Run Activate Namedrop again, then restart this agent to load the Namedrop hooks."'),
 ("H34 native setup explanation", "src/install.rs", "    if kind == crate::mux::MuxKind::Rex {\n        return json!({\n            \"status\": \"session_identity_missing\",", "    if kind == crate::mux::MuxKind::Rex || std::env::var_os(\"ND_PROBE_NEVER\").is_none() && false {\n        return json!({\n            \"status\": \"session_identity_missing\","),
]

prefixes=('H1 ', 'H2 ', 'H4 ', 'H6 ', 'H10 ', 'H11 ', 'H12 ', 'H13 ', 'H14 ', 'H21 ', 'H22 ', 'H27 ', 'H29 ', 'H31 ', 'H33 ')
cases=[]
for label,file,old,new in M:
    if label.startswith(prefixes):
        command=['cargo','test','--locked']
        if label.startswith(('H11 ','H12 ')):command+=['--bin','namedrop','words_host_tests']
        elif label.startswith('H13 '):command+=['--lib','words_host_tests']
        else:command+=['--test','words','host_pair_sites_are_reached']
        cases.append((label,file,old,new,command,"fn target_pane" if label.startswith("H12 ") else None))
cases += [
 ('F7 CLI prose','tests/words.rs','            &line.trim_start()["space".len()..]','            ""',['cargo','test','--locked','--test','words','nouns_preserve_protocol_identifiers'],None),
 ('F6 unused allowance','tests/words.rs','const AGENT_PLUGIN: [&str; 5] = [\n','const AGENT_PLUGIN: [&str; 6] = [\n    "namedrop plugin",\n',['cargo','test','--locked','--test','words','rex_words_sweep'],None),
 ('F3 registration','src/integration.rs','crate::mux::Endpoint::parse(session_socket_id)\n        .kind\n        .text("plugin-authored", "Namedrop-authored")','crate::mux::MuxKind::Herdr.text("plugin-authored", "Namedrop-authored")',['cargo','test','--locked','--test','words','host_pair_sites_are_reached'],None),
 ('F3 first tour','src/tour.rs','runtime.mux.endpoint().kind.text(','crate::mux::MuxKind::Herdr.text(',['cargo','test','--locked','--test','words','host_pair_sites_are_reached'],'fn show_for_first_agent'),
 ('F3 naming target','src/naming_ui.rs','kind.text(','namedrop::mux::MuxKind::Herdr.text(',['cargo','test','--locked','--bin','namedrop','words_host_tests'],'fn target_pane'),
 ('F3 installer','src/rex_ui/install.rs','directories.agent_config(),crate::mux::MuxKind::Rex)','directories.agent_config(),crate::mux::MuxKind::Herdr)',['cargo','test','--locked','--test','words','rex_words_sweep'],None),
 ('F3 hidden command','src/cli.rs','.hide(true)','', ['cargo','test','--locked','--test','words','host_pair_sites_are_reached'],None),
 ('P2 unused plugin allowance','src/health.rs','Start Codex or Claude Code in a Rex pane, then check again.','Restart the Namedrop plugin in a Rex pane, then check again.',['cargo','test','--locked','--test','words','rex_words_sweep'],None),
 ('S2 bare workspace','src/hook.rs','No named agents in this Rex session.','No named agents in this workspace.',['cargo','test','--locked','--test','words','rex_words_sweep'],None),
 ('F8 interrupt deadline','tests/install_script.py','                deadline = time.monotonic() + 30','                pass',['cargo','test','--locked','--test','install_script'],None),
]
results=[]
for label,file,old,new,command,scope in cases:
    if selected and not any(label.startswith(p) for p in selected):continue
    path=root/file;original=path.read_bytes();text=original.decode()
    start=text.index(scope) if scope else 0
    target=text.find(old,start)
    assert target>=0,(label,'mutation anchor absent',old)
    if not scope:assert text.count(old)==1,(label,'ambiguous anchor')
    path.write_text(text[:target]+text[target:].replace(old,new,1))
    try:
        logfile=out/(label.replace(' ','-')+'.log')
        with logfile.open('w') as log:
            child=subprocess.Popen(command,cwd=root,stdout=log,stderr=subprocess.STDOUT)
            print(label,'pid',child.pid,flush=True);code=child.wait()
        log=logfile.read_text()
        assert code!=0 and 'test result: FAILED' in log,(label,'survived or compile failure',code,log[-2000:])
        markers=['unused agent-plugin allowance', 'lost ', 'contains Rex replacement', 'Words violation','no Rex surface','assertion', 'AssertionError','panicked at']
        reason=next((line for line in log.splitlines() if any(m in line for m in markers)),None)
        assert reason,(label,'unrelated failure',log[-2000:])
        results.append(dict(mutation=label,command=command,exit=code,evidence=reason,log=str(logfile)))
        print(label,'DETECTED',reason[:220],flush=True)
    finally:path.write_bytes(original)
    (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
print(len(results),'runtime mutations detected',flush=True)
