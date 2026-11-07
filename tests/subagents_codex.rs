use namedrop::{
    AgentRuntime,
    subagent::{DiskFiles, Event, ParentIdentity, record, running},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const SESSION: &str = "01a1a7d9-a2ee-7c10-92ec-c387b315f877";
const ROOT_TURN: &str = "01a1a7dc-7924-7583-ae02-e2aa3cfd7f97";
const CHILDREN: [(&str, &str); 3] = [
    (
        "01a1a7dc-922b-7cf3-9707-4150e55865c2",
        "01a1a7dc-924a-73b0-b788-5e140de921a3",
    ),
    (
        "01a1a7dc-9cda-7212-bb38-a69c884878dc",
        "01a1a7dc-9cfe-7a92-b2ca-4b9821ffd4f9",
    ),
    (
        "01a1a7dc-aa22-7a42-828f-7aba3ae3f8a8",
        "01a1a7dc-aa43-73a2-aa0d-18bb2e67fc2b",
    ),
];

fn payload(edge: &str, turn: Option<&str>, id: Option<&str>) -> Value {
    json!({
        "session_id": SESSION,
        "hook_event_name": edge,
        "turn_id": turn,
        "agent_id": id,
        "agent_type": "default",
    })
}

struct State(PathBuf);

impl State {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "namedrop-codex-subagents-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn parent(&self) -> ParentIdentity {
        ParentIdentity::new("test-socket", "test-workspace", "test-pane", SESSION).unwrap()
    }

    fn record(&self, input: Value, at: i64) {
        let (session, event) =
            Event::parse(AgentRuntime::Codex, &input.to_string(), &DiskFiles).unwrap();
        assert_eq!(session, SESSION);
        record(&self.0, &self.parent(), event, at).unwrap();
    }

    fn ids(&self, now: i64) -> Vec<String> {
        running(&self.0, &self.parent(), now, None)
            .into_iter()
            .map(|run| run.id)
            .collect()
    }

    fn startup(&self) {
        self.record(payload("SessionStart", None, None), 100);
        self.record(payload("UserPromptSubmit", Some(ROOT_TURN), None), 110);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn codex_live_child_turns_show_three_runs_then_each_stop_removes_its_run() {
    let state = State::new();
    state.startup();
    for (index, (id, child_turn)) in CHILDREN.iter().enumerate() {
        state.record(
            payload("SubagentStart", Some(child_turn), Some(id)),
            120 + index as i64,
        );
        assert_eq!(state.ids(130).len(), index + 1);
    }
    for (index, (id, child_turn)) in CHILDREN.iter().enumerate() {
        state.record(
            payload("SubagentStop", Some(child_turn), Some(id)),
            140 + index as i64,
        );
        assert_eq!(
            state.ids(150),
            CHILDREN[index + 1..]
                .iter()
                .map(|(id, _)| id.to_string())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn codex_subagent_stop_matches_start_even_if_it_reports_root_turn() {
    let state = State::new();
    state.startup();
    let (id, child_turn) = CHILDREN[0];
    state.record(payload("SubagentStart", Some(child_turn), Some(id)), 120);
    assert_eq!(state.ids(125), [id]);
    state.record(payload("SubagentStop", Some(ROOT_TURN), Some(id)), 130);
    assert!(state.ids(140).is_empty());
}

#[test]
fn codex_parent_stop_and_native_idle_clear_unknown_parent_generation_runs() {
    for idle in [false, true] {
        let state = State::new();
        state.startup();
        let (id, child_turn) = CHILDREN[0];
        state.record(payload("SubagentStart", Some(child_turn), Some(id)), 120);
        assert_eq!(state.ids(125), [id]);
        if idle {
            assert!(running(&state.0, &state.parent(), 140, Some(130)).is_empty());
        } else {
            state.record(payload("Stop", Some(ROOT_TURN), None), 130);
        }
        assert!(state.ids(150).is_empty(), "idle={idle}");
    }
}

#[test]
fn codex_unknown_generation_needs_a_prompt_between_barrier_and_start() {
    for edge in ["SessionStart", "SessionEnd"] {
        for barrier_turn in [None, Some(ROOT_TURN)] {
            let state = State::new();
            state.record(payload("UserPromptSubmit", Some(ROOT_TURN), None), 100);
            let (id, child_turn) = CHILDREN[0];
            state.record(payload("SubagentStart", Some(child_turn), Some(id)), 110);
            state.record(payload(edge, barrier_turn, None), 120);
            let mut resume = payload("SessionStart", Some("new-root-turn"), None);
            resume["source"] = json!("resume");
            state.record(resume, 130);
            let (early_id, early_turn) = CHILDREN[1];
            state.record(
                payload("SubagentStart", Some(early_turn), Some(early_id)),
                140,
            );
            assert!(state.ids(145).is_empty(), "{edge} {barrier_turn:?}");
            state.record(
                payload("UserPromptSubmit", Some("new-root-turn"), None),
                150,
            );
            assert!(
                state.ids(155).is_empty(),
                "a later prompt cannot revive earlier starts"
            );
            let (new_id, new_turn) = CHILDREN[2];
            state.record(payload("SubagentStart", Some(new_turn), Some(new_id)), 160);
            assert_eq!(state.ids(170), [new_id], "{edge} {barrier_turn:?}");
        }
    }
}

#[test]
fn codex_root_prompt_and_stop_keep_the_root_turn_identity() {
    for edge in ["UserPromptSubmit", "Stop"] {
        let (_, event) = Event::parse(
            AgentRuntime::Codex,
            &payload(edge, Some(ROOT_TURN), None).to_string(),
            &DiskFiles,
        )
        .unwrap();
        match event {
            Event::Prompt { generation }
            | Event::ParentStop {
                generation: Some(generation),
                ..
            } => assert_eq!(generation, ROOT_TURN),
            _ => panic!("root event lost its turn identity"),
        }
    }
}

#[test]
fn codex_resume_after_parent_stop_needs_a_prompt_before_new_child_start() {
    let state = State::new();
    state.startup();
    let (id, turn) = CHILDREN[0];
    state.record(payload("SubagentStart", Some(turn), Some(id)), 120);
    assert_eq!(state.ids(125), [id]);
    state.record(payload("Stop", Some(ROOT_TURN), None), 130);
    let mut resume = payload("SessionStart", None, None);
    resume["source"] = json!("resume");
    state.record(resume, 140);
    let (early_id, early_turn) = CHILDREN[1];
    state.record(
        payload("SubagentStart", Some(early_turn), Some(early_id)),
        150,
    );
    assert!(state.ids(155).is_empty());
    state.record(
        payload("UserPromptSubmit", Some("next-root-turn"), None),
        160,
    );
    assert!(state.ids(165).is_empty());
    let (new_id, new_turn) = CHILDREN[2];
    state.record(payload("SubagentStart", Some(new_turn), Some(new_id)), 170);
    assert_eq!(state.ids(180), [new_id]);
}
