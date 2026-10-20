//! Which Herdr workspaces a leading run of Mentions reaches. Planning is pure
//! over one read of every space's names and labels; validity inside a space is
//! still answered by that space's own resolver.

use crate::{RecipientRun, SpaceId, SpaceScope, WorkspaceLabel};
use std::{collections::BTreeSet, fmt};

/// One read of the spaces in this Herdr session. The current space comes first,
/// then the others in Herdr's workspace order.
#[derive(Clone, Debug)]
pub(crate) struct SpaceDirectory {
    entries: Vec<SpaceEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpaceEntry {
    pub space: SpaceId,
    /// `None` only for the current space when no label had to be read.
    pub label: Option<WorkspaceLabel>,
    /// The label as Herdr shows it, made terminal-safe and bounded, for the one
    /// message that must tell two folded-equal labels apart.
    pub display: String,
    pub activated: bool,
    pub paused: bool,
    /// Agent aliases, group aliases, and tombstoned aliases. A tombstone counts
    /// as present so a retired local name keeps its retired error instead of
    /// silently reaching another workspace.
    pub names: BTreeSet<String>,
}

impl SpaceDirectory {
    pub(crate) fn new(here: SpaceEntry, others: Vec<SpaceEntry>) -> Self {
        let mut entries = vec![here];
        entries.extend(others);
        Self { entries }
    }

    pub(crate) fn here(&self) -> &SpaceEntry {
        &self.entries[0]
    }

    pub(crate) fn entry(&self, space: &SpaceId) -> Option<&SpaceEntry> {
        self.entries.iter().find(|entry| &entry.space == space)
    }

    fn others(&self) -> impl Iterator<Item = &SpaceEntry> {
        self.entries[1..].iter()
    }

    fn named(&self, label: &WorkspaceLabel) -> Result<&SpaceEntry, RouteRefusal> {
        let matches: Vec<&SpaceEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.label.as_ref() == Some(label))
            .collect();
        match matches.as_slice() {
            [] => Err(RouteRefusal::UnknownWorkspace {
                label: label.clone(),
                known: self
                    .entries
                    .iter()
                    .filter(|entry| entry.activated)
                    .filter_map(|entry| entry.label.clone())
                    .collect(),
            }),
            [entry] if !entry.activated => Err(RouteRefusal::NotActivated {
                label: label.clone(),
            }),
            [entry] => Ok(entry),
            many => Err(RouteRefusal::DuplicateLabel {
                label: label.clone(),
                workspaces: many.iter().map(|entry| entry.display.clone()).collect(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoutePlan {
    /// The current space's leg first when it has one.
    pub legs: Vec<RouteLeg>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteLeg {
    pub space: SpaceId,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteRefusal {
    /// Present nowhere. The text stays "unknown Mention: @x", which the hook's
    /// existing unresolved-recipient guidance answers.
    UnknownMention {
        alias: String,
    },
    /// Unqualified, absent here, present in two or more other workspaces.
    Ambiguous {
        choices: Vec<AmbiguousName>,
    },
    UnknownWorkspace {
        label: WorkspaceLabel,
        known: Vec<WorkspaceLabel>,
    },
    DuplicateLabel {
        label: WorkspaceLabel,
        workspaces: Vec<String>,
    },
    NotActivated {
        label: WorkspaceLabel,
    },
    /// Named scope: the alias is in none of the named workspaces.
    NotInNamedWorkspaces {
        alias: String,
        labels: Vec<WorkspaceLabel>,
    },
    /// Named scope: a named workspace receives none of the names.
    NothingForWorkspace {
        label: WorkspaceLabel,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AmbiguousName {
    pub alias: String,
    pub labels: Vec<WorkspaceLabel>,
}

impl fmt::Display for RouteRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMention { alias } => write!(formatter, "unknown Mention: @{alias}"),
            Self::Ambiguous { choices } => {
                for (index, choice) in choices.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("; ")?;
                    }
                    write!(
                        formatter,
                        "@{} is not named in this workspace but is named in {}",
                        choice.alias,
                        joined(&choice.labels, " and ")
                    )?;
                }
                formatter.write_str(
                    "; prefix the source with the workspace you mean, or #all for every one",
                )
            }
            Self::UnknownWorkspace { label, known } if known.is_empty() => {
                write!(formatter, "no workspace is labelled {label}")
            }
            Self::UnknownWorkspace { label, known } => write!(
                formatter,
                "no workspace is labelled {label}; activated workspaces: {}",
                joined(known, ", ")
            ),
            Self::DuplicateLabel { label, workspaces } => write!(
                formatter,
                "{label} matches more than one workspace ({}); rename one in Herdr",
                workspaces
                    .iter()
                    .map(|display| serde_json::Value::String(display.clone()).to_string())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            Self::NotActivated { label } => write!(
                formatter,
                "Namedrop is not activated in {label}; activate it there before sending"
            ),
            Self::NotInNamedWorkspaces { alias, labels } => write!(
                formatter,
                "@{alias} is not named in {}",
                joined(labels, " or ")
            ),
            Self::NothingForWorkspace { label } => {
                write!(formatter, "none of the Mentions is named in {label}")
            }
        }
    }
}

fn joined(labels: &[WorkspaceLabel], separator: &str) -> String {
    labels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

/// The resolution table, as code.
pub(crate) fn plan_route(
    run: &RecipientRun,
    directory: &SpaceDirectory,
) -> Result<RoutePlan, RouteRefusal> {
    let mut targets: Vec<(&SpaceEntry, &String)> = Vec::new();
    match &run.scope {
        SpaceScope::Unqualified => {
            let mut ambiguous = Vec::new();
            for alias in &run.aliases {
                let here = directory.here();
                if here.names.contains(alias) {
                    targets.push((here, alias));
                    continue;
                }
                let homes: Vec<&SpaceEntry> = directory
                    .others()
                    .filter(|entry| entry.activated && entry.names.contains(alias))
                    .collect();
                match homes.as_slice() {
                    [] => {
                        return Err(RouteRefusal::UnknownMention {
                            alias: alias.clone(),
                        });
                    }
                    [home] => targets.push((home, alias)),
                    homes => ambiguous.push(AmbiguousName {
                        alias: alias.clone(),
                        labels: homes.iter().filter_map(|home| home.label.clone()).collect(),
                    }),
                }
            }
            if !ambiguous.is_empty() {
                return Err(RouteRefusal::Ambiguous { choices: ambiguous });
            }
        }
        SpaceScope::Named(labels) => {
            let named = labels
                .iter()
                .map(|label| directory.named(label))
                .collect::<Result<Vec<_>, _>>()?;
            for alias in &run.aliases {
                let homes: Vec<&SpaceEntry> = named
                    .iter()
                    .copied()
                    .filter(|entry| entry.names.contains(alias))
                    .collect();
                if homes.is_empty() {
                    return Err(RouteRefusal::NotInNamedWorkspaces {
                        alias: alias.clone(),
                        labels: labels.clone(),
                    });
                }
                targets.extend(homes.into_iter().map(|home| (home, alias)));
            }
            if let Some((_, label)) = named.iter().zip(labels).find(|(entry, _)| {
                !targets
                    .iter()
                    .any(|(target, _)| target.space == entry.space)
            }) {
                return Err(RouteRefusal::NothingForWorkspace {
                    label: label.clone(),
                });
            }
        }
        SpaceScope::All => {
            for alias in &run.aliases {
                let before = targets.len();
                targets.extend(
                    directory
                        .entries
                        .iter()
                        .filter(|entry| entry.activated && entry.names.contains(alias))
                        .map(|entry| (entry, alias)),
                );
                if targets.len() == before {
                    return Err(RouteRefusal::UnknownMention {
                        alias: alias.clone(),
                    });
                }
            }
        }
    }
    let legs = directory
        .entries
        .iter()
        .filter_map(|entry| {
            let aliases: Vec<String> = targets
                .iter()
                .filter(|(target, _)| target.space == entry.space)
                .map(|(_, alias)| (*alias).clone())
                .collect();
            (!aliases.is_empty()).then(|| RouteLeg {
                space: entry.space.clone(),
                aliases,
            })
        })
        .collect();
    Ok(RoutePlan { legs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(token: &str) -> WorkspaceLabel {
        WorkspaceLabel::from_token(token).unwrap()
    }

    fn entry(id: &str, herdr_label: &str, names: &[&str]) -> SpaceEntry {
        SpaceEntry {
            space: SpaceId::new(id),
            label: Some(WorkspaceLabel::from_herdr(herdr_label)),
            display: herdr_label.to_owned(),
            activated: true,
            paused: false,
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// Here is `ws1`. `lead` lives here and in ws2 and ws3; `tester` only in
    /// ws2; `ops` in ws2 and ws3; `reviewers` is a group in ws2; `old` is a
    /// local tombstone that also names a live agent in ws2.
    fn directory() -> SpaceDirectory {
        SpaceDirectory::new(
            entry("w1", "ws1", &["lead", "old"]),
            vec![
                entry(
                    "w2",
                    "My Space",
                    &["lead", "tester", "ops", "reviewers", "old"],
                ),
                entry("w3", "ws3", &["lead", "ops"]),
                SpaceEntry {
                    activated: false,
                    ..entry("w4", "dormant", &["lead", "ghost"])
                },
            ],
        )
    }

    fn run(scope: SpaceScope, aliases: &[&str]) -> RecipientRun {
        RecipientRun {
            scope,
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        }
    }

    fn legs(plan: Result<RoutePlan, RouteRefusal>) -> Vec<(String, Vec<String>)> {
        plan.unwrap()
            .legs
            .into_iter()
            .map(|leg| (leg.space.as_str().to_owned(), leg.aliases))
            .collect()
    }

    fn leg(space: &str, aliases: &[&str]) -> (String, Vec<String>) {
        (
            space.to_owned(),
            aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        )
    }

    #[test]
    fn unqualified_resolution_table() {
        let directory = directory();
        let plan =
            |aliases: &[&str]| plan_route(&run(SpaceScope::Unqualified, aliases), &directory);
        assert_eq!(legs(plan(&["lead"])), [leg("w1", &["lead"])], "local wins");
        assert_eq!(
            legs(plan(&["old"])),
            [leg("w1", &["old"])],
            "a local tombstone stays local and keeps its retired error"
        );
        assert_eq!(
            legs(plan(&["tester"])),
            [leg("w2", &["tester"])],
            "one remote home"
        );
        assert_eq!(
            legs(plan(&["reviewers"])),
            [leg("w2", &["reviewers"])],
            "a group alias follows the same table"
        );
        assert_eq!(
            plan(&["ops"]),
            Err(RouteRefusal::Ambiguous {
                choices: vec![AmbiguousName {
                    alias: "ops".to_owned(),
                    labels: vec![label("my-space"), label("ws3")],
                }],
            })
        );
        assert_eq!(
            plan(&["ops", "nobody"]),
            Err(RouteRefusal::UnknownMention {
                alias: "nobody".to_owned()
            }),
            "an unknown name wins over a question the person cannot answer"
        );
        assert_eq!(
            plan(&["ghost"]),
            Err(RouteRefusal::UnknownMention {
                alias: "ghost".to_owned()
            }),
            "an inactive workspace is not a home"
        );
        assert_eq!(
            legs(plan(&["tester", "lead"])),
            [leg("w1", &["lead"]), leg("w2", &["tester"])],
            "here's leg comes first"
        );
    }

    #[test]
    fn named_resolution_table() {
        let directory = directory();
        let plan = |labels: &[&str], aliases: &[&str]| {
            plan_route(
                &run(
                    SpaceScope::Named(labels.iter().map(|token| label(token)).collect()),
                    aliases,
                ),
                &directory,
            )
        };
        assert_eq!(
            legs(plan(&["my-space"], &["lead"])),
            [leg("w2", &["lead"])],
            "My Space matches #my-space, and a named scope skips the local name"
        );
        assert_eq!(
            legs(plan(&["ws3", "my-space"], &["lead", "ops"])),
            [leg("w2", &["lead", "ops"]), leg("w3", &["lead", "ops"])]
        );
        assert_eq!(legs(plan(&["ws1"], &["lead"])), [leg("w1", &["lead"])]);
        assert!(matches!(
            plan(&["nowhere"], &["lead"]),
            Err(RouteRefusal::UnknownWorkspace { known, .. })
                if known == [label("ws1"), label("my-space"), label("ws3")]
        ));
        assert_eq!(
            plan(&["dormant"], &["lead"]),
            Err(RouteRefusal::NotActivated {
                label: label("dormant")
            })
        );
        assert_eq!(
            plan(&["ws3"], &["tester"]),
            Err(RouteRefusal::NotInNamedWorkspaces {
                alias: "tester".to_owned(),
                labels: vec![label("ws3")],
            })
        );
        assert_eq!(
            plan(&["my-space", "ws3"], &["tester"]),
            Err(RouteRefusal::NothingForWorkspace {
                label: label("ws3")
            })
        );
    }

    #[test]
    fn duplicate_folded_labels_name_both_workspaces() {
        let directory = SpaceDirectory::new(
            entry("w1", "ws1", &[]),
            vec![
                entry("w2", "Dash Board", &["lead"]),
                entry("w3", "dash-board", &["lead"]),
            ],
        );
        let refusal = plan_route(
            &run(SpaceScope::Named(vec![label("dash-board")]), &["lead"]),
            &directory,
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "#dash-board matches more than one workspace (\"Dash Board\" and \"dash-board\"); \
             rename one in Herdr"
        );
    }

    #[test]
    fn all_reaches_every_activated_workspace_with_the_name() {
        let directory = directory();
        assert_eq!(
            legs(plan_route(&run(SpaceScope::All, &["lead"]), &directory)),
            [
                leg("w1", &["lead"]),
                leg("w2", &["lead"]),
                leg("w3", &["lead"])
            ]
        );
        assert_eq!(
            plan_route(&run(SpaceScope::All, &["ghost"]), &directory),
            Err(RouteRefusal::UnknownMention {
                alias: "ghost".to_owned()
            })
        );
    }

    #[test]
    fn an_unknown_mention_keeps_the_text_the_hook_matches() {
        assert_eq!(
            RouteRefusal::UnknownMention {
                alias: "x".to_owned()
            }
            .to_string(),
            "unknown Mention: @x"
        );
    }
}
