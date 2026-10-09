//! Project skill descriptor and host instruction contracts.
use astra_skills::loader::parse_skill_md;
use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn project_claude_and_agent_skill_bodies_stay_in_sync() {
    let agent_root = astra_core::test_paths::workspace_path(".agent/skills");
    let claude_root = astra_core::test_paths::workspace_path(".claude/skills");

    let agent_names = skill_dir_names(&agent_root);
    let claude_names = skill_dir_names(&claude_root);
    assert_eq!(
        claude_names, agent_names,
        ".claude/skills and .agent/skills must expose the same project skill set"
    );

    for skill_name in agent_names {
        let (agent_manifest, agent_body) = parsed_skill(&agent_root, &skill_name);
        let (claude_manifest, claude_body) = parsed_skill(&claude_root, &skill_name);
        assert_eq!(
            claude_body, agent_body,
            "{skill_name}: .claude and .agent skill instruction bodies drifted"
        );

        let agent_tools = agent_manifest.allowed_tools.clone();
        let claude_tools = claude_manifest.allowed_tools.clone();
        let mut agent_contract = serde_json::to_value(agent_manifest).unwrap();
        let mut claude_contract = serde_json::to_value(claude_manifest).unwrap();
        agent_contract["allowed_tools"] = serde_json::json!([]);
        claude_contract["allowed_tools"] = serde_json::json!([]);
        assert_eq!(
            claude_contract, agent_contract,
            "{skill_name}: compatibility roots may only differ in host tool availability"
        );
        if skill_name == "review_changes" {
            assert_eq!(
                agent_tools
                    .iter()
                    .filter(|tool| !claude_tools.contains(tool))
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["agent_fanout"],
                "review_changes may expose only the Agent-host parallelism capability as a root-specific delta"
            );
        } else {
            assert_eq!(
                claude_tools, agent_tools,
                "{skill_name}: allowed tools drifted between compatibility roots"
            );
        }
    }
}

#[test]
fn project_skills_have_one_descriptor() {
    for root in [".agent/skills", ".claude/skills"] {
        let root = astra_core::test_paths::workspace_path(root);
        for skill_name in skill_dir_names(&root) {
            let skill_dir = root.join(&skill_name);
            for legacy_sidecar in ["manifest.yaml", "metadata.json"] {
                assert!(
                    !skill_dir.join(legacy_sidecar).exists(),
                    "{skill_name}: {legacy_sidecar} duplicates canonical SKILL.md frontmatter"
                );
            }
        }
    }
}

fn skill_dir_names(root: &Path) -> BTreeSet<String> {
    std::fs::read_dir(root)
        .unwrap_or_else(|e| panic!("read skill root {}: {e}", root.display()))
        .filter_map(|entry| {
            let entry = entry.unwrap_or_else(|e| panic!("read skill dir entry: {e}"));
            let file_type = entry
                .file_type()
                .unwrap_or_else(|e| panic!("read file type for {}: {e}", entry.path().display()));
            if !file_type.is_dir() {
                return None;
            }
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                return None;
            }
            Some(entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

fn parsed_skill(root: &Path, skill_name: &str) -> (astra_skills::manifest::SkillManifest, String) {
    let path = root.join(skill_name).join("SKILL.md");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    parse_skill_md(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}
