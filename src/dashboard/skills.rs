use super::snapshot::SafeText;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 4096;
const MAX_DIRECTORIES: usize = 512;
const MAX_SKILLS: usize = 512;
const MAX_BYTES: u64 = 32 * 1024;
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_CACHE: usize = 32;
const TTL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SkillRuntime {
    Claude,
    Codex,
}

impl SkillRuntime {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SkillSource {
    Personal,
    Project,
    Plugin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Skill {
    name: String,
    description: String,
    source: SkillSource,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DiscoveryContext {
    runtime: SkillRuntime,
    cwd: PathBuf,
    home: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
}

impl DiscoveryContext {
    fn environment(runtime: SkillRuntime, cwd: &Path) -> Option<Self> {
        if !cwd.is_absolute() {
            return None;
        }
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let canonical = |path: PathBuf| fs::canonicalize(&path).unwrap_or(path);
        Some(Self {
            runtime,
            cwd: fs::canonicalize(cwd).ok()?,
            claude: canonical(
                std::env::var_os("CLAUDE_CONFIG_DIR")
                    .map_or_else(|| home.join(".claude"), PathBuf::from),
            ),
            codex: canonical(
                std::env::var_os("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from),
            ),
            home: canonical(home),
        })
    }
}

struct Cached {
    at: Instant,
    skills: Vec<Skill>,
}

#[derive(Default)]
pub(super) struct SkillCatalog {
    cache: Mutex<Vec<(DiscoveryContext, Cached)>>,
}

impl SkillCatalog {
    pub(super) fn for_pane(&self, runtime: SkillRuntime, cwd: &Path) -> Vec<Skill> {
        let Some(context) = DiscoveryContext::environment(runtime, cwd) else {
            return Vec::new();
        };
        self.get(context, Instant::now())
    }

    fn get(&self, context: DiscoveryContext, now: Instant) -> Vec<Skill> {
        if let Some(skills) = self.cache.lock().ok().and_then(|cache| {
            cache.iter().find_map(|(key, entry)| {
                (key == &context && now.saturating_duration_since(entry.at) < TTL)
                    .then(|| entry.skills.clone())
            })
        }) {
            return skills;
        }
        let skills = discover(&context);
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|(key, entry)| {
                key != &context && now.saturating_duration_since(entry.at) < TTL
            });
            if cache.len() >= MAX_CACHE {
                cache.remove(0);
            }
            cache.push((
                context,
                Cached {
                    at: now,
                    skills: skills.clone(),
                },
            ));
        }
        skills
    }
}

struct Scan {
    entries: usize,
    directories: usize,
    visited: HashSet<PathBuf>,
    skills: BTreeMap<String, Skill>,
}

impl Scan {
    fn new() -> Self {
        Self {
            entries: 0,
            directories: 0,
            visited: HashSet::new(),
            skills: BTreeMap::new(),
        }
    }

    fn children(&mut self, path: &Path) -> Vec<PathBuf> {
        if self.directories >= MAX_DIRECTORIES || self.entries >= MAX_ENTRIES {
            return Vec::new();
        }
        self.directories += 1;
        let Ok(canonical) = fs::canonicalize(path) else {
            return Vec::new();
        };
        if !self.visited.insert(canonical) {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(path) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for entry in entries.take(MAX_ENTRIES - self.entries) {
            self.entries += 1;
            if let Ok(entry) = entry {
                paths.push(entry.path());
            }
        }
        paths.sort();
        paths
    }

    fn tree(&mut self, path: &Path, source: SkillSource, plugin: Option<&str>, depth: usize) {
        if depth > 8 || self.skills.len() >= MAX_SKILLS || self.entries >= MAX_ENTRIES {
            return;
        }
        self.entries += 1;
        let skill = path.join("SKILL.md");
        if skill.is_file() {
            let fallback = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            self.add(&skill, fallback, source, plugin, false);
            return;
        }
        for child in self.children(path) {
            if child.is_dir() {
                self.tree(&child, source, plugin, depth + 1);
            }
        }
    }

    fn commands(
        &mut self,
        root: &Path,
        path: &Path,
        source: SkillSource,
        plugin: Option<&str>,
        depth: usize,
    ) {
        if depth > 8 || self.skills.len() >= MAX_SKILLS {
            return;
        }
        for child in self.children(path) {
            if child.is_dir() {
                self.commands(root, &child, source, plugin, depth + 1);
            } else if child.extension().is_some_and(|extension| extension == "md") {
                let relative = child
                    .strip_prefix(root)
                    .unwrap_or(&child)
                    .with_extension("");
                let name = relative
                    .iter()
                    .filter_map(|part| part.to_str())
                    .collect::<Vec<_>>()
                    .join(":");
                self.add(&child, &name, source, plugin, true);
            }
        }
    }

    fn add(
        &mut self,
        path: &Path,
        fallback: &str,
        source: SkillSource,
        plugin: Option<&str>,
        legacy: bool,
    ) {
        if self.skills.len() >= MAX_SKILLS {
            return;
        }
        let Some(header) = frontmatter(path, legacy) else {
            return;
        };
        if header.user_invocable == Some(false) {
            return;
        }
        let name = header.name.as_deref().unwrap_or(fallback);
        if !safe_name(name) || plugin.is_some_and(|name| !safe_name(name)) {
            return;
        }
        let name = plugin.map_or_else(|| name.to_owned(), |plugin| format!("{plugin}:{name}"));
        let description = SafeText::from_untrusted(
            &super::http::strip_terminal_sequences(&header.description),
            2048,
        )
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
        self.skills.entry(name.clone()).or_insert(Skill {
            name,
            description,
            source,
        });
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 160
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

#[derive(Default, Deserialize)]
struct Header {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<bool>,
}

fn regular(path: &Path) -> Option<std::fs::File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

fn frontmatter(path: &Path, legacy: bool) -> Option<Header> {
    let file = regular(path)?;
    let mut reader = BufReader::new(file.take(MAX_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.trim_end() != "---" {
        return legacy.then(Header::default);
    }
    let mut yaml = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line.trim_end() == "---" {
            return serde_yaml_ng::from_str(&yaml).ok();
        }
        yaml.push_str(&line);
    }
}

fn read_config(path: &Path) -> Option<String> {
    let mut text = String::new();
    regular(path)?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= MAX_CONFIG_BYTES as usize).then_some(text)
}

fn json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_config(path)?).ok()
}

fn projects(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for path in cwd.ancestors().take(64) {
        paths.push(path.to_owned());
        if path.join(".git").exists() {
            break;
        }
    }
    paths
}

fn discover(context: &DiscoveryContext) -> Vec<Skill> {
    let mut scan = Scan::new();
    let projects = projects(&context.cwd);
    match context.runtime {
        SkillRuntime::Claude => {
            scan.tree(
                &context.claude.join("skills"),
                SkillSource::Personal,
                None,
                0,
            );
            for project in &projects {
                scan.tree(
                    &project.join(".claude/skills"),
                    SkillSource::Project,
                    None,
                    0,
                );
            }
            scan.commands(
                &context.claude.join("commands"),
                &context.claude.join("commands"),
                SkillSource::Personal,
                None,
                0,
            );
            for project in &projects {
                let path = project.join(".claude/commands");
                scan.commands(&path, &path, SkillSource::Project, None, 0);
            }
            claude_plugins(context, &projects, &mut scan);
        }
        SkillRuntime::Codex => {
            for project in &projects {
                scan.tree(
                    &project.join(".agents/skills"),
                    SkillSource::Project,
                    None,
                    0,
                );
            }
            scan.tree(
                &context.home.join(".agents/skills"),
                SkillSource::Personal,
                None,
                0,
            );
            scan.tree(
                &context.codex.join("skills"),
                SkillSource::Personal,
                None,
                0,
            );
            codex_plugins(context, &mut scan);
        }
    }
    scan.skills.into_values().collect()
}

fn plugin_tree(scan: &mut Scan, root: &Path, name: &str, manifest: &Path, commands: bool) {
    if scan.entries >= MAX_ENTRIES || scan.skills.len() >= MAX_SKILLS {
        return;
    }
    scan.entries += 1;
    let manifest = json(manifest);
    let paths = manifest.as_ref().and_then(|value| value.get("skills"));
    let paths: Vec<&str> = match paths {
        Some(Value::String(path)) => vec![if path == "./" || path == "." {
            "skills"
        } else {
            path
        }],
        Some(Value::Array(paths)) => paths
            .iter()
            .filter_map(Value::as_str)
            .take(32)
            .map(|path| {
                if path == "./" || path == "." {
                    "skills"
                } else {
                    path
                }
            })
            .collect(),
        _ => vec!["skills"],
    };
    for path in paths {
        scan.tree(&root.join(path), SkillSource::Plugin, Some(name), 0);
    }
    if commands {
        let path = root.join("commands");
        scan.commands(&path, &path, SkillSource::Plugin, Some(name), 0);
    }
}

fn claude_plugins(context: &DiscoveryContext, projects: &[PathBuf], scan: &mut Scan) {
    let mut enabled = BTreeMap::new();
    let mut settings = vec![context.claude.join("settings.json")];
    for path in projects.iter().rev() {
        settings.push(path.join(".claude/settings.json"));
        settings.push(path.join(".claude/settings.local.json"));
    }
    for path in settings {
        if let Some(Value::Object(plugins)) =
            json(&path).and_then(|mut value| value.get_mut("enabledPlugins").map(Value::take))
        {
            for (name, value) in plugins.into_iter().take(MAX_ENTRIES) {
                if let Some(value) = value.as_bool() {
                    enabled.insert(name, value);
                }
            }
        }
    }
    let Some(installed) = json(&context.claude.join("plugins/installed_plugins.json")) else {
        return;
    };
    let Some(plugins) = installed.get("plugins").and_then(Value::as_object) else {
        return;
    };
    for (id, records) in plugins.iter().take(MAX_DIRECTORIES) {
        if enabled.get(id) != Some(&true) {
            continue;
        }
        let name = id.split('@').next().unwrap_or(id);
        let Some(records) = records.as_array() else {
            continue;
        };
        for record in records.iter().take(16) {
            if let Some(project) = record.get("projectPath").and_then(Value::as_str)
                && !projects.iter().any(|path| path == Path::new(project))
            {
                continue;
            }
            if let Some(root) = record.get("installPath").and_then(Value::as_str) {
                let root = Path::new(root);
                plugin_tree(
                    scan,
                    root,
                    name,
                    &root.join(".claude-plugin/plugin.json"),
                    true,
                );
            }
        }
    }
}

fn codex_plugins(context: &DiscoveryContext, scan: &mut Scan) {
    let Some(config) = read_config(&context.codex.join("config.toml"))
        .and_then(|config| config.parse::<toml_edit::DocumentMut>().ok())
    else {
        return;
    };
    let Some(plugins) = config
        .get("plugins")
        .and_then(toml_edit::Item::as_table_like)
    else {
        return;
    };
    for (id, plugin) in plugins.iter().take(MAX_DIRECTORIES) {
        if plugin.get("enabled").and_then(toml_edit::Item::as_bool) != Some(true) {
            continue;
        }
        let Some((name, market)) = id.split_once('@') else {
            continue;
        };
        if !safe_name(name) || !safe_name(market) {
            continue;
        }
        let root = context.codex.join("plugins/cache").join(market).join(name);
        let versions = scan.children(&root);
        if let Some((_, version)) = versions
            .into_iter()
            .filter_map(|path| {
                let version = semver::Version::parse(path.file_name()?.to_str()?).ok()?;
                path.join(".codex-plugin/plugin.json")
                    .is_file()
                    .then_some((version, path))
            })
            .max_by(|a, b| a.0.cmp(&b.0))
        {
            plugin_tree(
                scan,
                &version,
                name,
                &version.join(".codex-plugin/plugin.json"),
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "namedrop-skills-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("home")).unwrap();
            fs::create_dir_all(path.join("xdg")).unwrap();
            fs::create_dir_all(path.join("repo/sub")).unwrap();
            fs::create_dir_all(path.join("repo/.git")).unwrap();
            Self(path)
        }
        fn context(&self, runtime: SkillRuntime) -> DiscoveryContext {
            DiscoveryContext {
                runtime,
                cwd: self.0.join("repo/sub"),
                home: self.0.join("home"),
                claude: self.0.join("claude-config"),
                codex: self.0.join("codex-home"),
            }
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        fn skill(&self, path: &str, name: &str) {
            self.write(
                &format!("{path}/SKILL.md"),
                &format!(
                    "---\nname: {name}\ndescription: A fixture skill\n---\nBody must not appear."
                ),
            );
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn names(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|skill| skill.name.as_str()).collect()
    }

    #[test]
    fn runtimes_load_only_their_roots_with_project_and_system_skills() {
        let f = Fixture::new();
        f.skill("claude-config/skills/claude", "claude-only");
        f.skill("codex-home/skills/codex", "codex-only");
        f.skill("codex-home/skills/.system/core", "system");
        f.skill("home/.agents/skills/shared", "shared");
        symlink(f.0.join("home/.agents/skills/shared"), {
            fs::create_dir_all(f.0.join("claude-config/skills")).unwrap();
            f.0.join("claude-config/skills/shared")
        })
        .unwrap();
        f.skill("repo/.agents/skills/project", "codex-project");
        f.skill("repo/.claude/skills/project", "claude-project");
        f.skill(".agents/skills/outside", "outside-repo");
        let claude = discover(&f.context(SkillRuntime::Claude));
        let codex = discover(&f.context(SkillRuntime::Codex));
        assert_eq!(names(&claude), ["claude-only", "claude-project", "shared"]);
        assert_eq!(
            names(&codex),
            ["codex-only", "codex-project", "shared", "system"]
        );
        assert_eq!(
            codex
                .iter()
                .find(|s| s.name == "codex-project")
                .unwrap()
                .source,
            SkillSource::Project
        );
    }

    #[test]
    fn yaml_validation_visibility_and_plain_descriptions() {
        let f = Fixture::new();
        f.write("codex-home/skills/good/SKILL.md", "---\nname: quoted\ndescription: >-\n  First line\n  second line\n---\nDo not read this body");
        f.write(
            "codex-home/skills/hidden/SKILL.md",
            "---\nname: hidden\nuser-invocable: false\n---",
        );
        f.write(
            "codex-home/skills/malformed/SKILL.md",
            "---\nname: [bad\n---",
        );
        f.write("codex-home/skills/missing/SKILL.md", "No frontmatter");
        f.write(
            "codex-home/skills/unsafe/SKILL.md",
            "---\nname: 'bad name'\n---",
        );
        f.write(
            "codex-home/skills/type/SKILL.md",
            "---\nname: wrong-type\ndescription: [not, a, string]\n---",
        );
        f.write(
            "codex-home/skills/long/SKILL.md",
            &format!("---\nname: long\ndescription: '{}'\n---", "x".repeat(400)),
        );
        let skills = discover(&f.context(SkillRuntime::Codex));
        assert_eq!(names(&skills), ["long", "quoted"]);
        assert_eq!(skills[0].description.len(), 300);
        assert_eq!(skills[1].description, "First line second line");
        assert!(!serde_json::to_string(&skills).unwrap().contains("body"));
    }

    #[test]
    fn claude_commands_and_plugins_follow_enablement_and_skill_precedence() {
        let f = Fixture::new();
        f.skill("claude-config/skills/shared", "shared");
        f.skill("repo/.claude/skills/shared", "shared");
        f.write(
            "claude-config/commands/shared.md",
            "---\ndescription: Command duplicate\n---\n",
        );
        f.write(
            "claude-config/commands/nested/old.md",
            "Legacy command without frontmatter",
        );
        f.skill("plugin/skills/tool", "tool");
        f.skill("disabled/skills/nope", "nope");
        f.write(
            "claude-config/settings.json",
            r#"{"enabledPlugins":{"demo@market":true,"disabled@market":false}}"#,
        );
        f.write("claude-config/plugins/installed_plugins.json", &serde_json::json!({"version":2,"plugins":{"demo@market":[{"installPath":f.0.join("plugin"),"scope":"user"}],"disabled@market":[{"installPath":f.0.join("disabled")}]}}).to_string());
        let skills = discover(&f.context(SkillRuntime::Claude));
        assert_eq!(names(&skills), ["demo:tool", "nested:old", "shared"]);
        assert_eq!(skills[0].source, SkillSource::Plugin);
        assert_eq!(skills[2].source, SkillSource::Personal);
        assert_eq!(skills[2].description, "A fixture skill");
    }

    #[test]
    fn codex_enabled_plugins_use_manifest_roots_and_namespace() {
        let f = Fixture::new();
        f.write("codex-home/config.toml", "[plugins.\"demo@market\"]\nenabled = true\n[plugins.\"disabled@market\"]\nenabled = false\n");
        let plugin = "codex-home/plugins/cache/market/demo/1.0.0";
        f.write(
            &format!("{plugin}/.codex-plugin/plugin.json"),
            r#"{"name":"demo","skills":["./other"]}"#,
        );
        f.skill(&format!("{plugin}/other/tool"), "tool");
        f.skill(&format!("{plugin}/skills/skip"), "skip");
        for version in ["2.0.0", "10.0.0"] {
            let root = format!("codex-home/plugins/cache/market/demo/{version}");
            f.write(
                &format!("{root}/.codex-plugin/plugin.json"),
                r#"{"name":"demo","skills":"./other"}"#,
            );
            f.skill(
                &format!("{root}/other/tool"),
                if version == "10.0.0" {
                    "newest"
                } else {
                    "older"
                },
            );
        }
        f.skill(
            "codex-home/plugins/cache/market/disabled/1.0.0/skills/disabled",
            "disabled",
        );
        let skills = discover(&f.context(SkillRuntime::Codex));
        assert_eq!(names(&skills), ["demo:newest"]);
        assert_eq!(skills[0].source, SkillSource::Plugin);
    }

    #[test]
    fn symlink_cycles_and_nonregular_skill_files_do_not_block_discovery() {
        let f = Fixture::new();
        f.skill("codex-home/skills/good", "good");
        symlink(
            f.0.join("codex-home/skills"),
            f.0.join("codex-home/skills/loop"),
        )
        .unwrap();
        fs::create_dir_all(f.0.join("codex-home/skills/pipe")).unwrap();
        let pipe = std::ffi::CString::new(
            f.0.join("codex-home/skills/pipe/SKILL.md")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: pipe is a valid, NUL-terminated fixture path.
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        assert_eq!(names(&discover(&f.context(SkillRuntime::Codex))), ["good"]);
    }

    #[test]
    fn invalid_entries_and_oversized_headers_consume_bounded_work() {
        let f = Fixture::new();
        f.write(
            "codex-home/skills/huge/SKILL.md",
            &format!(
                "---\nname: huge\ndescription: {}\n---",
                "a".repeat(MAX_BYTES as usize)
            ),
        );
        assert!(discover(&f.context(SkillRuntime::Codex)).is_empty());
        let mut scan = Scan::new();
        let root = f.0.join("many");
        fs::create_dir(&root).unwrap();
        for index in 0..MAX_ENTRIES + 5 {
            fs::write(root.join(format!("invalid-{index}")), "").unwrap();
        }
        scan.tree(&root, SkillSource::Personal, None, 0);
        assert_eq!(scan.entries, MAX_ENTRIES);
        assert!(scan.skills.is_empty());
        let mut scan = Scan::new();
        for index in 0..MAX_SKILLS + 5 {
            f.skill(&format!("valid/{index}"), &format!("skill-{index}"));
            scan.add(
                &f.0.join(format!("valid/{index}/SKILL.md")),
                "",
                SkillSource::Personal,
                None,
                false,
            );
        }
        assert_eq!(scan.skills.len(), MAX_SKILLS);
    }

    #[test]
    fn cache_refreshes_after_ttl_and_separates_runtime_cwd_and_config() {
        let f = Fixture::new();
        let cache = SkillCatalog::default();
        let now = Instant::now();
        let context = f.context(SkillRuntime::Codex);
        assert!(cache.get(context.clone(), now).is_empty());
        f.skill("codex-home/skills/new", "new");
        assert!(
            cache
                .get(context.clone(), now + Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(names(&cache.get(context.clone(), now + TTL)), ["new"]);
        assert!(
            cache
                .get(f.context(SkillRuntime::Claude), now + TTL)
                .is_empty()
        );
        let mut other = context.clone();
        other.codex = f.0.join("other-config");
        assert!(cache.get(other, now + TTL).is_empty());
        for index in 0..MAX_CACHE + 4 {
            let mut other = context.clone();
            other.cwd = f.0.join(format!("cwd-{index}"));
            cache.get(other, now + TTL);
        }
        assert_eq!(cache.cache.lock().unwrap().len(), MAX_CACHE);
    }
}
